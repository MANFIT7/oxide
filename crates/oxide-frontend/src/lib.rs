//! The shell contract.
//!
//! Every Oxide UI implements [`Frontend`]: it is handed an [`EngineHandle`] to
//! submit ops and an [`Event`] receiver to render. The terminal UI and the
//! future desktop GUI are two implementations of this one trait, so switching
//! between `oxide tui` and `oxide gui` is just choosing which `Frontend` to run
//! against the identical engine.

use async_trait::async_trait;
use oxide_core::EngineHandle;
use oxide_protocol::Event;
use tokio::sync::mpsc;

#[async_trait]
pub trait Frontend {
    /// Human-facing name ("tui", "gui", "headless").
    fn name(&self) -> &str;

    /// Take over I/O until the user quits. Implementations submit [`Op`]s via
    /// `handle` and consume `events` until the channel closes.
    async fn run(
        self: Box<Self>,
        handle: EngineHandle,
        events: mpsc::Receiver<Event>,
    ) -> anyhow::Result<()>;
}
