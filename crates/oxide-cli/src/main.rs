//! `oxide` — the multitool entry point.
//!
//! Every subcommand drives the same [`oxide_core`] engine; they differ only in
//! which [`Frontend`] (or none) they attach. `tui` runs the terminal UI today;
//! `gui` is reserved for the desktop frontend (Fase 7) over the identical engine.

use anyhow::Result;
use clap::{Parser, Subcommand};
use oxide_config::Config;
use oxide_frontend::Frontend;
use oxide_harness::Registry;
use oxide_tui::Tui;

#[derive(Parser)]
#[command(name = "oxide", version, about = "Rust-native AI coding agent")]
struct Cli {
    /// Override the active harness (e.g. default, hermes).
    #[arg(long, global = true)]
    harness: Option<String>,

    /// Override the provider backend (echo, mock, openai, anthropic).
    #[arg(long, global = true)]
    provider: Option<String>,

    /// Seed history from the most recent session.
    #[arg(long, global = true)]
    resume: bool,

    /// Re-enable permission prompts (default is bypass, like `codex --yolo` /
    /// `claude --dangerously-skip-permissions`).
    #[arg(long, global = true)]
    safe: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Launch the interactive terminal UI (default).
    Tui,
    /// Run a single prompt headless and print the event stream.
    Exec {
        /// The prompt to run.
        prompt: String,
        /// Auto-approve every tool call (non-interactive).
        #[arg(long)]
        yes: bool,
    },
    /// Launch the Rust-native desktop command center.
    Gui,
    /// Inspect available harnesses.
    Harness {
        #[command(subcommand)]
        action: HarnessAction,
    },
}

#[derive(Subcommand)]
enum HarnessAction {
    /// List every registered harness (builtin + manifest).
    List,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,chromiumoxide=off,tungstenite=off".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let mut config = Config::load()?;
    if let Some(h) = cli.harness {
        config.harness = h;
    }
    if let Some(p) = cli.provider {
        config.provider = p;
    }
    if cli.resume {
        config.resume = true;
    }
    // Bypass permissions by default (no prompts), unless --safe is passed.
    if !cli.safe {
        config.approval_policy = oxide_protocol::ApprovalPolicy::Never;
    }

    match cli.command.unwrap_or(Command::Tui) {
        // The GUI owns the platform event loop and must run on the main thread,
        // outside any tokio runtime — so it is dispatched here directly.
        Command::Gui => {
            // Terminal launch in a real project dir → use it. Finder launch
            // (cwd "/") → always start at the Open-folder welcome, ignoring any
            // persisted workspace, so the sidebar is empty until a folder is picked.
            match std::env::current_dir() {
                Ok(cwd) if cwd != std::path::Path::new("/") => {
                    if config.workspace.is_none() {
                        config.workspace = Some(cwd);
                    }
                }
                _ => config.workspace = None,
            }
            oxide_gui::run(config)
        }
        // Everything else runs on a manually-built async runtime.
        other => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move {
                match other {
                    Command::Tui => run_tui(config).await,
                    Command::Exec { prompt, yes } => run_exec(config, prompt, yes).await,
                    Command::Harness { action } => match action {
                        HarnessAction::List => list_harnesses(&config),
                    },
                    Command::Gui => unreachable!(),
                }
            })
        }
    }
}

/// Headless single-turn runner: submit one prompt, print every event, optionally
/// auto-approve tool calls. Drives the same engine as the TUI/GUI.
async fn run_exec(config: Config, prompt: String, yes: bool) -> Result<()> {
    use oxide_protocol::{ApprovalDecision, Event, Op};

    let (handle, mut events) = oxide_core::spawn(config)?;
    handle.submit(Op::UserTurn { text: prompt }).await?;

    while let Some(ev) = events.recv().await {
        match ev {
            Event::Ready { harness } => println!("[ready] harness={harness}"),
            Event::TurnStarted { turn } => println!("[{turn}] started"),
            Event::AgentMessageDelta { text, .. } => print!("{text}"),
            Event::ReasoningDelta { .. } => {}
            Event::ApprovalRequested {
                request_id,
                tool,
                summary,
            } => {
                println!("\n[approval] {tool}: {summary}");
                let decision = if yes {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Reject
                };
                println!("[approval] -> {decision:?}");
                handle
                    .submit(Op::ApprovalResponse {
                        request_id,
                        decision,
                    })
                    .await?;
            }
            Event::ToolCallBegin { tool, .. } => println!("\n[tool] {tool} …"),
            Event::ToolCallEnd {
                tool, output, ok, ..
            } => {
                println!("[tool] {tool} ok={ok}: {output}")
            }
            Event::PatchApplied { path, .. } => println!("[patch] {path}"),
            Event::CheckpointCreated { id, label, .. } => {
                println!("[checkpoint] #{id}: {label}")
            }
            Event::RewindDone { id, restored } => {
                println!("[rewind] #{id} restored {restored} file(s)")
            }
            Event::Compacted { dropped, tokens } => {
                println!("[compacted] dropped {dropped} msg(s), ~{tokens} tokens")
            }
            Event::TokensUsed { input, output, .. } => {
                println!("\n[tokens] in={input} out={output}")
            }
            Event::ContextWindow { limit } => println!("[context] window={limit}"),
            Event::FileDiff { path, .. } => println!("[diff] {path}"),
            Event::HookFired { hook, command, blocked } => {
                println!("[hook] {hook}: {command}{}", if blocked { " (blocked)" } else { "" })
            }
            Event::QuestionAsked { question, options, .. } => {
                println!("[question] {question}");
                for (i, o) in options.iter().enumerate() {
                    println!("  {}. {o}", i + 1);
                }
            }
            Event::HarnessChanged { id } => println!("[harness] {id}"),
            Event::McpServerStatus {
                name,
                status,
                tool_count,
                detail,
                ..
            } => println!("[mcp] {name} {status} tools={tool_count}: {detail}"),
            Event::BrowserTargetChanged { url, note, .. } => {
                println!("[browser] target={url} note={note}")
            }
            Event::BrowserSnapshotRequested { url, note, .. } => {
                println!("[browser] snapshot={url} note={note}")
            }
            Event::Info { text } => println!("[info] {text}"),
            Event::Error { message } => eprintln!("[error] {message}"),
            Event::TurnFinished { .. } => break,
            Event::Shutdown => break,
        }
    }
    handle.submit(Op::Shutdown).await.ok();
    println!();
    Ok(())
}

async fn run_tui(config: Config) -> Result<()> {
    let harness = config.harness.clone();
    let (handle, events) = oxide_core::spawn(config)?;
    let tui = Box::new(Tui::new(harness));
    tui.run(handle, events).await
}

fn list_harnesses(config: &Config) -> Result<()> {
    let mut registry = Registry::with_builtins();
    if let Some(dir) = &config.harness_dir {
        let _ = registry.load_dir(dir);
    }
    println!("Available harnesses:");
    for id in registry.ids() {
        let h = registry.get(&id).unwrap();
        let active = if id == config.harness {
            " (active)"
        } else {
            ""
        };
        println!("  {:<12} {}{}", id, h.display_name(), active);
    }
    Ok(())
}
