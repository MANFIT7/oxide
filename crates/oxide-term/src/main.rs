//! oxide-term — native GPU terminal (Milestones 1–2).
//!
//! Pipeline: portable-pty (shell) → alacritty_terminal::Term (VTE emulation +
//! cell grid) → glyphon (GPU text via wgpu) + a small wgpu quad pipeline (cell
//! backgrounds + cursor) in a winit window. On macOS wgpu selects the Metal
//! backend automatically. JetBrainsMono Nerd Font is bundled.
//!
//! Done: per-cell fg color + bold, per-cell bg color, an inverse block cursor,
//! scrollback (mouse wheel), and keyboard input incl. Ctrl-combos. Selection +
//! Oxide-window integration are the remaining steps.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor, Processor, Rgb};

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::Window;

const NERD_FONT: &[u8] =
    include_bytes!("../../oxide-gui/assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf");
const FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 18.0;
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 4.0;
const DEFAULT_FG: Rgb = Rgb {
    r: 228,
    g: 231,
    b: 223,
};
const DEFAULT_BG: Rgb = Rgb {
    r: 13,
    g: 12,
    b: 11,
};

/// Wakes the winit loop when the PTY produced output.
enum UserEvent {
    PtyData,
    PtyExited,
}

#[derive(Clone)]
struct Listener;
impl EventListener for Listener {
    fn send_event(&self, _event: TermEvent) {}
}

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

/// One run of same-styled glyphs (built per visible row).
struct Run {
    text: String,
    fg: Rgb,
    bold: bool,
}

/// A solid-color cell rectangle (background or cursor), in pixel coords.
#[derive(Clone, Copy)]
struct Quad {
    col: usize,
    line: usize,
    color: Rgb,
}

/// Everything needed to draw one frame, extracted from the terminal grid.
struct Frame {
    runs: Vec<Run>,
    quads: Vec<Quad>,
}

/// Standard xterm 256-color palette fallback (used when the Term's palette slot
/// is unset — a fresh Term doesn't preload a full theme).
fn palette_256(idx: usize) -> Rgb {
    match idx {
        0 => Rgb { r: 0, g: 0, b: 0 },
        1 => Rgb {
            r: 205,
            g: 49,
            b: 49,
        },
        2 => Rgb {
            r: 13,
            g: 188,
            b: 121,
        },
        3 => Rgb {
            r: 229,
            g: 229,
            b: 16,
        },
        4 => Rgb {
            r: 36,
            g: 114,
            b: 200,
        },
        5 => Rgb {
            r: 188,
            g: 63,
            b: 188,
        },
        6 => Rgb {
            r: 17,
            g: 168,
            b: 205,
        },
        7 => Rgb {
            r: 229,
            g: 229,
            b: 229,
        },
        8 => Rgb {
            r: 102,
            g: 102,
            b: 102,
        },
        9 => Rgb {
            r: 241,
            g: 76,
            b: 76,
        },
        10 => Rgb {
            r: 35,
            g: 209,
            b: 139,
        },
        11 => Rgb {
            r: 245,
            g: 245,
            b: 67,
        },
        12 => Rgb {
            r: 59,
            g: 142,
            b: 234,
        },
        13 => Rgb {
            r: 214,
            g: 112,
            b: 214,
        },
        14 => Rgb {
            r: 41,
            g: 184,
            b: 219,
        },
        15 => Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        16..=231 => {
            let i = idx - 16;
            let conv = |v: usize| -> u8 {
                if v == 0 {
                    0
                } else {
                    (v * 40 + 55) as u8
                }
            };
            Rgb {
                r: conv((i / 36) % 6),
                g: conv((i / 6) % 6),
                b: conv(i % 6),
            }
        }
        _ => {
            let v = (8 + (idx.saturating_sub(232)) * 10).min(238) as u8;
            Rgb { r: v, g: v, b: v }
        }
    }
}

/// Resolve a terminal cell color to concrete RGB.
fn resolve(c: TermColor, palette: &Colors, is_fg: bool) -> Rgb {
    match c {
        TermColor::Spec(rgb) => rgb,
        TermColor::Indexed(i) => palette[i as usize].unwrap_or_else(|| palette_256(i as usize)),
        TermColor::Named(n) => palette[n].unwrap_or_else(|| match n {
            NamedColor::Background => DEFAULT_BG,
            NamedColor::Foreground => DEFAULT_FG,
            other => {
                let idx = other as usize;
                if idx < 256 {
                    palette_256(idx)
                } else if is_fg {
                    DEFAULT_FG
                } else {
                    DEFAULT_BG
                }
            }
        }),
    }
}

struct Pty {
    term: Arc<Mutex<Term<Listener>>>,
    parser: Arc<Mutex<Processor>>,
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    child_reaped: bool,
    rx: Receiver<Vec<u8>>,
    size: TermSize,
}

impl Pty {
    fn spawn(
        size: TermSize,
        proxy: EventLoopProxy<UserEvent>,
        cwd: Option<String>,
        command: Vec<String>,
    ) -> anyhow::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system.openpty(portable_pty::PtySize {
            rows: size.lines as u16,
            cols: size.cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        // Run the requested program (e.g. codex / claude), or fall back to $SHELL.
        let mut cmd = if let Some((prog, rest)) = command.split_first() {
            let mut c = portable_pty::CommandBuilder::new(prog);
            for a in rest {
                c.arg(a);
            }
            c
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            portable_pty::CommandBuilder::new(shell)
        };
        let dir = match cwd {
            Some(cwd) => {
                let path = std::path::PathBuf::from(cwd);
                if !path.is_dir() {
                    anyhow::bail!(
                        "terminal working directory does not exist: {}",
                        path.display()
                    );
                }
                path
            }
            None => std::env::current_dir()?,
        };
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let term = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &size,
            Listener,
        )));
        let parser = Arc::new(Mutex::new(Processor::new()));

        let (tx, rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = sync_channel(64);
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
            let _ = proxy.send_event(UserEvent::PtyExited);
        });

        Ok(Self {
            term,
            parser,
            writer,
            master: pair.master,
            child,
            child_reaped: false,
            rx,
            size,
        })
    }

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

    fn child_exited(&mut self) -> bool {
        if self.child_reaped {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.child_reaped = true;
                true
            }
            _ => false,
        }
    }

    fn shutdown(&mut self) {
        if self.child_reaped {
            return;
        }
        if !self.child_exited() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.child_reaped = true;
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Any keypress jumps back to the live screen.
        self.term.lock().unwrap().scroll_display(Scroll::Bottom);
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn scroll(&mut self, lines: i32) {
        self.term
            .lock()
            .unwrap()
            .scroll_display(Scroll::Delta(lines));
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

    /// Walk the visible grid into draw runs + background/cursor quads.
    fn frame(&self) -> Frame {
        let term = self.term.lock().unwrap();
        let content = term.renderable_content();
        let palette = content.colors;
        let cursor_pt = content.cursor.point;
        let cursor_visible = !matches!(content.cursor.shape, CursorShape::Hidden);

        let mut runs: Vec<Run> = Vec::new();
        let mut quads: Vec<Quad> = Vec::new();
        let mut cur_line: i32 = i32::MIN;

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let line = point.line.0; // 0-based within the visible region
            let col = point.column.0;
            if line < 0 {
                continue;
            }
            let uline = line as usize;

            let mut fg = resolve(cell.fg, palette, true);
            let mut bg = resolve(cell.bg, palette, false);
            let bold = cell.flags.intersects(Flags::BOLD | Flags::BOLD_ITALIC);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let is_cursor = cursor_visible
                && line == cursor_pt.line.0
                && col == cursor_pt.column.0
                && matches!(
                    content.cursor.shape,
                    CursorShape::Block | CursorShape::HollowBlock
                );
            if is_cursor {
                std::mem::swap(&mut fg, &mut bg);
            }

            // Background quad when not the default bg.
            if bg.r != DEFAULT_BG.r || bg.g != DEFAULT_BG.g || bg.b != DEFAULT_BG.b {
                quads.push(Quad {
                    col,
                    line: uline,
                    color: bg,
                });
            }

            // Break rows with newlines; merge consecutive same-styled cells.
            let need_newline = cur_line >= 0 && line != cur_line;
            if need_newline {
                if let Some(last) = runs.last_mut() {
                    for _ in 0..(line - cur_line) {
                        last.text.push('\n');
                    }
                }
            }
            cur_line = line;
            let same_style = runs
                .last()
                .map(|r| r.fg == fg && r.bold == bold)
                .unwrap_or(false);
            if same_style && !need_newline {
                runs.last_mut().unwrap().text.push(cell.c);
            } else {
                runs.push(Run {
                    text: cell.c.to_string(),
                    fg,
                    bold,
                });
            }
        }

        Frame { runs, quads }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    _pad: [f32; 2],
}

/// Minimal solid-color quad pipeline for cell backgrounds + cursor.
struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    vbuf_cap: u64,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    n: u32,
}

impl QuadRenderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad"),
            source: wgpu::ShaderSource::Wgsl(QUAD_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            }],
        });
        let vbuf_cap = 4096 * std::mem::size_of::<Vertex>() as u64;
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: vbuf_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            vbuf,
            vbuf_cap,
            ubuf,
            bind,
            n: 0,
        }
    }

    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        quads: &[Quad],
        res: [f32; 2],
        cell_w: f32,
    ) {
        queue.write_buffer(
            &self.ubuf,
            0,
            bytemuck::bytes_of(&Uniforms {
                resolution: res,
                _pad: [0.0, 0.0],
            }),
        );
        let mut verts: Vec<Vertex> = Vec::with_capacity(quads.len() * 6);
        for q in quads {
            let x0 = PAD_X + q.col as f32 * cell_w;
            let y0 = PAD_Y + q.line as f32 * LINE_HEIGHT;
            let x1 = x0 + cell_w;
            let y1 = y0 + LINE_HEIGHT;
            let c = [
                q.color.r as f32 / 255.0,
                q.color.g as f32 / 255.0,
                q.color.b as f32 / 255.0,
                1.0,
            ];
            for p in [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]] {
                verts.push(Vertex { pos: p, color: c });
            }
        }
        self.n = verts.len() as u32;
        if verts.is_empty() {
            return;
        }
        let bytes = bytemuck::cast_slice(&verts);
        let needed = bytes.len() as u64;
        if needed > self.vbuf_cap {
            self.vbuf_cap = needed.next_power_of_two();
            self.vbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: self.vbuf_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.vbuf, 0, bytes);
    }

    fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.n == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.vbuf.slice(..));
        pass.draw(0..self.n, 0..1);
    }
}

const QUAD_WGSL: &str = r#"
struct Uniforms { resolution: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) color: vec4<f32> };
@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    let ndc = vec2<f32>(pos.x / u.resolution.x * 2.0 - 1.0, 1.0 - pos.y / u.resolution.y * 2.0);
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;

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
    quads: QuadRenderer,
    /// The loaded font's REAL family name (queried, not guessed).
    family: String,
    /// Measured monospace cell advance in px (so bg quads/cursor align to glyphs).
    cell_w: f32,
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
        // Use the loaded font's REAL family name. Guessing the string is fragile —
        // a mismatch makes cosmic-text fall back to a default face, so the Nerd
        // glyphs vanish or the text renders in the wrong font (a likely "weird").
        let family = font_system
            .db()
            .faces()
            .last()
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_else(|| FONT_FAMILY.to_string());
        // Measure the real monospace advance so background quads + the cursor line
        // up with the glyphs (the 0.6-em guess drifts on some faces).
        let cell_w = {
            let mut probe = TextBuffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            probe.set_text(
                &mut font_system,
                "MMMMMMMMMMMMMMMMMMMM",
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            probe.shape_until_scroll(&mut font_system, false);
            probe
                .layout_runs()
                .next()
                .map(|r| r.line_w / 20.0)
                .filter(|w| *w > 0.5)
                .unwrap_or(FONT_SIZE * 0.6)
        };
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
        let quads = QuadRenderer::new(&device, format);

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
            quads,
            family,
            cell_w,
            window,
        }
    }

    fn set_runs(&mut self, runs: &[Run]) {
        let fam = self.family.clone();
        let spans: Vec<(&str, Attrs)> = runs
            .iter()
            .map(|r| {
                let attrs = Attrs::new()
                    .family(Family::Name(&fam))
                    .color(Color::rgb(r.fg.r, r.fg.g, r.fg.b))
                    .weight(if r.bold { Weight::BOLD } else { Weight::NORMAL });
                (r.text.as_str(), attrs)
            })
            .collect();
        self.text_buffer.set_rich_text(
            &mut self.font_system,
            spans,
            &Attrs::new().family(Family::Name(&fam)),
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

    fn render(&mut self, frame: &Frame) {
        self.set_runs(&frame.runs);
        let res = [
            self.surface_config.width as f32,
            self.surface_config.height as f32,
        ];
        self.quads
            .prepare(&self.device, &self.queue, &frame.quads, res, self.cell_w);

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
                left: PAD_X,
                top: PAD_Y,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.surface_config.width as i32,
                    bottom: self.surface_config.height as i32,
                },
                default_color: Color::rgb(DEFAULT_FG.r, DEFAULT_FG.g, DEFAULT_FG.b),
                custom_glyphs: &[],
            }],
            &mut self.swash_cache,
        );
        if prepared.is_err() {
            return;
        }
        let surface_tex = match self.surface.get_current_texture() {
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
        let view = surface_tex
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
                            r: DEFAULT_BG.r as f64 / 255.0,
                            g: DEFAULT_BG.g as f64 / 255.0,
                            b: DEFAULT_BG.b as f64 / 255.0,
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
            self.quads.render(&mut pass);
            let _ = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass);
        }
        self.queue.submit(Some(encoder.finish()));
        surface_tex.present();
        self.atlas.trim();
    }
}

struct App {
    gpu: Option<Gpu>,
    pty: Option<Pty>,
    proxy: EventLoopProxy<UserEvent>,
    mods: ModifiersState,
    cwd: Option<String>,
    cmd: Vec<String>,
}

impl App {
    fn grid_size(cell_w: f32, w: u32, h: u32) -> TermSize {
        let cols = ((w as f32 - PAD_X * 2.0) / cell_w).floor().max(1.0) as usize;
        let lines = ((h as f32 - PAD_Y * 2.0) / LINE_HEIGHT).floor().max(1.0) as usize;
        TermSize { cols, lines }
    }

    fn redraw(&mut self) {
        if let (Some(pty), Some(gpu)) = (self.pty.as_mut(), self.gpu.as_mut()) {
            pty.pump();
            let frame = pty.frame();
            gpu.render(&frame);
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let title = match self.cmd.first() {
            Some(prog) => format!("oxide-term — {prog}"),
            None => "oxide-term".to_string(),
        };
        let attrs = Window::default_attributes()
            .with_inner_size(LogicalSize::new(900.0, 560.0))
            .with_title(title);
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        // Bring the new terminal window to the front + give it keyboard focus.
        window.focus_window();
        let gpu = pollster::block_on(Gpu::new(window.clone(), event_loop));
        let size = window.inner_size();
        let grid = Self::grid_size(gpu.cell_w, size.width, size.height);
        let pty = Pty::spawn(grid, self.proxy.clone(), self.cwd.clone(), self.cmd.clone())
            .expect("spawn pty");
        self.gpu = Some(gpu);
        self.pty = Some(pty);
        window.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyData => {
                if let Some(gpu) = self.gpu.as_ref() {
                    gpu.window.request_redraw();
                }
            }
            UserEvent::PtyExited => {
                if let Some(pty) = self.pty.as_mut() {
                    pty.shutdown();
                }
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(pty) = self.pty.as_mut() {
                    pty.shutdown();
                }
                event_loop.exit();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(pty)) = (self.gpu.as_mut(), self.pty.as_mut()) {
                    gpu.resize(size.width, size.height);
                    pty.resize(Self::grid_size(gpu.cell_w, size.width, size.height));
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(pty) = self.pty.as_mut() {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                        MouseScrollDelta::PixelDelta(p) => {
                            (p.y / LINE_HEIGHT as f64).round() as i32
                        }
                    };
                    if lines != 0 {
                        pty.scroll(lines);
                        if let Some(gpu) = self.gpu.as_ref() {
                            gpu.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state.is_pressed() {
                    let bytes = key_to_bytes(&event.logical_key, &event.text, self.mods);
                    if !bytes.is_empty() {
                        if let Some(pty) = self.pty.as_mut() {
                            pty.write_input(&bytes);
                            if let Some(gpu) = self.gpu.as_ref() {
                                gpu.window.request_redraw();
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}

fn key_to_bytes(
    key: &Key,
    text: &Option<winit::keyboard::SmolStr>,
    mods: ModifiersState,
) -> Vec<u8> {
    // Ctrl-letter → control byte (Ctrl-C = 0x03, etc.).
    if mods.control_key() {
        if let Key::Character(s) = key {
            if let Some(ch) = s.chars().next() {
                let lower = ch.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    return vec![(lower as u8) & 0x1f];
                }
                match lower {
                    '[' => return vec![0x1b],
                    '\\' => return vec![0x1c],
                    ']' => return vec![0x1d],
                    _ => {}
                }
            }
        }
    }
    match key {
        Key::Named(NamedKey::Enter) => vec![b'\r'],
        Key::Named(NamedKey::Backspace) => vec![0x7f],
        Key::Named(NamedKey::Tab) => vec![b'\t'],
        Key::Named(NamedKey::Escape) => vec![0x1b],
        Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
        Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
        Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
        Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
        Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
        Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
        Key::Named(NamedKey::Delete) => b"\x1b[3~".to_vec(),
        Key::Named(NamedKey::Space) => vec![b' '],
        _ => text
            .as_ref()
            .map(|t| t.as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

fn parse_args(
    args: impl IntoIterator<Item = String>,
) -> anyhow::Result<(Option<String>, Vec<String>)> {
    let mut args = args.into_iter().peekable();
    let mut cwd = None;
    if args.peek().is_some_and(|arg| arg == "--cwd") {
        args.next();
        let value = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("--cwd requires a directory"))?;
        if !std::path::Path::new(&value).is_dir() {
            anyhow::bail!("terminal working directory does not exist: {value}");
        }
        cwd = Some(value);
    }
    Ok((cwd, args.collect()))
}

fn main() -> anyhow::Result<()> {
    // Usage: oxide-term [--cwd DIR] [PROGRAM ARGS...]   (default PROGRAM = $SHELL)
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.as_slice() == ["--version"] {
        println!("oxide-term {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(raw_args.as_slice(), [arg] if arg == "--help" || arg == "-h") {
        println!("Usage: oxide-term [--cwd DIR] [PROGRAM ARGS...]\n\nRuns PROGRAM, or $SHELL when omitted, in a native GPU terminal.");
        return Ok(());
    }
    let (cwd, cmd) = parse_args(raw_args)?;

    #[cfg(target_os = "macos")]
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    // macOS: a bare spawned binary (not a .app bundle) defaults to an accessory
    // process that never becomes the key window — so keyboard input never
    // reaches it and the window can open behind the launcher. Make it a Regular
    // foreground app and pull it in front on launch so it takes keyboard focus.
    #[cfg(target_os = "macos")]
    {
        builder.with_activation_policy(ActivationPolicy::Regular);
        builder.with_activate_ignoring_other_apps(true);
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App {
        gpu: None,
        pty: None,
        proxy,
        mods: ModifiersState::empty(),
        cwd,
        cmd,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_rejects_missing_or_invalid_cwd() {
        assert!(parse_args(["--cwd".to_string()]).is_err());
        assert!(parse_args(["--cwd".to_string(), "/definitely/missing".to_string()]).is_err());
    }

    #[test]
    fn parse_args_preserves_program_arguments() {
        let cwd = std::env::current_dir().unwrap();
        let (parsed_cwd, command) = parse_args([
            "--cwd".to_string(),
            cwd.display().to_string(),
            "printf".to_string(),
            "hello".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed_cwd.as_deref(), Some(cwd.to_string_lossy().as_ref()));
        assert_eq!(command, ["printf", "hello"]);
    }
}
