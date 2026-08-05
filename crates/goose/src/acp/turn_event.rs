//! Live mirror of an ACP-driven turn.
//!
//! Produced by the run-side ACP server when an interactive `goose run` opts into
//! `GOOSE_RUN_SERVE_ACP_PORT`. The interactive TUI consumes these events so that
//! dispatcher-initiated turns (`session/prompt` arriving over the loopback ACP
//! WebSocket) render in the terminal the same way user-typed input does, instead
//! of being invisible while the ACP handler streams the reply straight back to
//! the bridge.

use crate::agents::AgentEvent;

/// One tick of an ACP-driven turn, tapped at the ACP handler and replayed to the
/// owning `goose run` TUI.
///
/// The ACP handler retains the single live `reply()` stream and ships it over
/// the WebSocket as today; these events are a read-only duplicate for display.
/// Steering a turn already in flight is out of scope.
#[derive(Clone, Debug)]
pub enum AcpTurnEvent {
    /// A `session/prompt` arrived. `text` is the dispatcher's prompt rendered as
    /// plain text; the TUI shows it as the user-side bubble of the turn.
    PromptReceived { session_id: String, text: String },
    /// Duplicate of an event from the agent's reply stream. The TUI renders it
    /// through the same path as a user-initiated turn.
    StreamEvent(Box<AgentEvent>),
    /// The turn completed normally.
    TurnDone { session_id: String },
    /// The turn ended in error or was cancelled.
    TurnFailed { session_id: String, error: String },
}

/// Channel side the run-side ACP server taps into.
pub type AcpTurnTap = tokio::sync::mpsc::Sender<AcpTurnEvent>;
