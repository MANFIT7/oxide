//! oxide-term — native GPU terminal (Milestone 1).
//!
//! Pipeline: portable-pty (shell) → alacritty_terminal::Term (VTE emulation,
//! the grid) → glyphon (GPU text via wgpu) in a winit window. On macOS wgpu
//! selects the Metal backend automatically. Nerd Font is loaded so powerline /
//! dev-icon glyphs render. M1 renders the visible grid as monospaced text
//! (per-cell color + cursor + selection come in M1.5/M2); keyboard input is
//! forwarded to the PTY.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

/// The bundled monospace Nerd Font (reuse the Oxide GUI asset).
const NERD_FONT: &[u8] =
    include_bytes!("../../oxide-gui/assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf");
const FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 18.0;
/// Approx monospace advance width at FONT_SIZE (tuned to JetBrains Mono ~0.6 em).
const CELL_W: f32 = FONT_SIZE * 0.6;

/// Wakes the winit loop when the PTY produced output.
enum UserEvent {
    PtyData,
}

/// No-op terminal event sink (M1 ignores bell/title/clipboard events).
#[derive(Clone)]
struct Listener;
impl EventListener for Listener {
    fn send_event(&self, _event: TermEvent) {}
}

/// Grid dimensions handed to `Term`.
#[derive(Clone, Copy)]
struct TermSize {
    cols: usize,
    lines: usize,
}
impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Owns the PTY + emulation; shared between the reader thread and the UI loop.
struct Pty {
    term: Arc<Mutex<Term<Listener>>>,
    parser: Arc<Mutex<Processor>>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    rx: Receiver<Vec<u8>>,
    size: TermSize,
}

impl Pty {
    fn spawn(size: TermSize, proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system.openpty(portable_pty::PtySize {
            rows: size.lines as u16,
            cols: size.cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let mut cmd = portable_pty::CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        let _child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let term = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &size,
            Listener,
        )));
        let parser = Arc::new(Mutex::new(Processor::new()));

        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        let _ = proxy.send_event(UserEvent::PtyData);
                    }
                }
            }
        });

        Ok(Self {
            term,
            parser,
            writer,
            master: pair.master,
            rx,
            size,
        })
    }

    /// Drain any PTY output and advance the emulator.
    fn pump(&mut self) {
        let chunks: Vec<Vec<u8>> = self.rx.try_iter().collect();
        if chunks.is_empty() {
            return;
        }
        let mut term = self.term.lock().unwrap();
        let mut parser = self.parser.lock().unwrap();
        for chunk in chunks {
            parser.advance(&mut *term, &chunk);
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn resize(&mut self, size: TermSize) {
        self.size = size;
        let _ = self.master.resize(portable_pty::PtySize {
            rows: size.lines as u16,
            cols: size.cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.term.lock().unwrap().resize(size);
    }

    /// Snapshot the visible grid as plain text (M1: monochrome).
    fn snapshot_text(&self) -> String {
        let term = self.term.lock().unwrap();
        let grid = term.grid();
        let mut out = String::with_capacity(self.size.lines * (self.size.cols + 1));
        for line in 0..self.size.lines as i32 {
            for col in 0..self.size.cols {
                let cell = &grid[Line(line)][Column(col)];
                out.push(cell.c);
            }
            out.push('\n');
        }
        out
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: TextBuffer,
    window: Arc<Window>,
}

impl Gpu {
    async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("no GPU adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("no device");
        let surface = instance.create_surface(window.clone()).expect("surface");
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        font_system.db_mut().load_font_data(NERD_FONT.to_vec());
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let mut text_buffer =
            TextBuffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        text_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );

        Self {
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            text_buffer,
            window,
        }
    }

    fn set_text(&mut self, text: &str) {
        self.text_buffer.set_text(
            &mut self.font_system,
            text,
            &Attrs::new().family(Family::Name(FONT_FAMILY)),
            Shaping::Advanced,
            None,
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.surface_config.width = w.max(1);
        self.surface_config.height = h.max(1);
        self.surface.configure(&self.device, &self.surface_config);
        self.text_buffer
            .set_size(&mut self.font_system, Some(w as f32), Some(h as f32));
    }

    fn render(&mut self) {
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );
        let prepared = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [TextArea {
                buffer: &self.text_buffer,
                left: 6.0,
                top: 4.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.surface_config.width as i32,
                    bottom: self.surface_config.height as i32,
                },
                default_color: Color::rgb(228, 231, 223),
                custom_glyphs: &[],
            }],
            &mut self.swash_cache,
        );
        if prepared.is_err() {
            return;
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            _ => {
                self.window.request_redraw();
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.04,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let _ = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
    }
}

struct App {
    gpu: Option<Gpu>,
    pty: Option<Pty>,
    proxy: EventLoopProxy<UserEvent>,
}

impl App {
    /// Compute the grid size from the window pixels + cell metrics.
    fn grid_size(w: u32, h: u32) -> TermSize {
        let cols = ((w as f32 - 12.0) / CELL_W).floor().max(1.0) as usize;
        let lines = ((h as f32 - 8.0) / LINE_HEIGHT).floor().max(1.0) as usize;
        TermSize { cols, lines }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_inner_size(LogicalSize::new(900.0, 560.0))
            .with_title("oxide-term");
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let gpu = pollster::block_on(Gpu::new(window.clone(), event_loop));
        let size = window.inner_size();
        let grid = Self::grid_size(size.width, size.height);
        let pty = Pty::spawn(grid, self.proxy.clone()).expect("spawn pty");
        self.gpu = Some(gpu);
        self.pty = Some(pty);
        window.request_redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let (Some(pty), Some(gpu)) = (self.pty.as_mut(), self.gpu.as_mut()) {
            pty.pump();
            let text = pty.snapshot_text();
            gpu.set_text(&text);
            gpu.window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(pty)) = (self.gpu.as_mut(), self.pty.as_mut()) {
                    gpu.resize(size.width, size.height);
                    pty.resize(Self::grid_size(size.width, size.height));
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state.is_pressed() {
                    if let Some(pty) = self.pty.as_mut() {
                        let bytes = key_to_bytes(&event.logical_key, &event.text);
                        if !bytes.is_empty() {
                            pty.write_input(&bytes);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let (Some(pty), Some(gpu)) = (self.pty.as_mut(), self.gpu.as_mut()) {
                    pty.pump();
                    let text = pty.snapshot_text();
                    gpu.set_text(&text);
                    gpu.render();
                }
            }
            _ => {}
        }
    }
}

/// Map a key press to the bytes the PTY expects.
fn key_to_bytes(key: &Key, text: &Option<winit::keyboard::SmolStr>) -> Vec<u8> {
    match key {
        Key::Named(NamedKey::Enter) => vec![b'\r'],
        Key::Named(NamedKey::Backspace) => vec![0x7f],
        Key::Named(NamedKey::Tab) => vec![b'\t'],
        Key::Named(NamedKey::Escape) => vec![0x1b],
        Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
        Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
        Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
        Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
        Key::Named(NamedKey::Space) => vec![b' '],
        _ => text
            .as_ref()
            .map(|t| t.as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App {
        gpu: None,
        pty: None,
        proxy,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
