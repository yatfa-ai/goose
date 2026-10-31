//! Live mirror of dispatcher-driven turns in the interactive TUI.
//!
//! When `goose run` lifts a run-side ACP server (`GOOSE_RUN_SERVE_ACP_PORT`),
//! the ACP handler streams its reply straight back over the loopback WebSocket.
//! Without this mirror the operator attached over tmux sees a frozen pane, so
//! the handler taps every turn onto a channel this module drains.
//!
//! The mirror renders on its own thread while the interactive loop sits in
//! `readline`, which means it must not write to stdout directly: rustyline owns
//! the prompt line in raw mode and tracks the cursor itself, so a foreign write
//! leaves it redrawing from a stale position, swallowing typed characters.
//! Everything is therefore rendered under [`output::capture`] and handed to the
//! editor's external printer, which erases the line, prints, and redraws it.
//!
//! The printer ends every message with a newline, so the mirror only hands it
//! complete lines (see [`LineCoalescer`]); streamed deltas would otherwise each
//! land on a line of their own.
//!
//! Capture also suppresses spinners, which is what the mirror wants anyway: a
//! self-repainting widget cannot be handed over as a string, and progress bars
//! are transient status rather than content — the shell output and tool results
//! they preview arrive again as tool responses.

use console::Color;
use goose::acp::AcpTurnEvent;
use goose::agents::AgentEvent;
use rustyline::ExternalPrinter;
use std::io::Write;
use tokio::sync::mpsc::UnboundedReceiver;

use super::{handle_mcp_notification, output, streaming_buffer};

/// Drain the turn mirror on a dedicated thread, printing through `printer`.
///
/// A thread rather than a task: the rendering path keeps its markdown, theme and
/// spinner state in thread locals, and a task migrating between runtime workers
/// would split that state across threads — and could land on the interactive
/// loop's own thread and stop its thinking indicator.
///
/// `printer` is `None` when stdio is not a terminal, where there is no line
/// editor to collide with and the mirror can write straight to stdout.
pub fn spawn<P>(mut events: UnboundedReceiver<AcpTurnEvent>, mut printer: Option<P>, debug: bool)
where
    P: ExternalPrinter + Send + 'static,
{
    std::thread::spawn(move || {
        let mut markdown_buffer = streaming_buffer::MarkdownBuffer::new();
        let mut progress_bars = output::McpSpinners::new();
        let mut thinking_header_shown = false;
        let mut lines = LineCoalescer::default();

        // Returns false once the printer is gone and the mirror should stop.
        let mut deliver = |text: String| -> bool {
            if text.is_empty() {
                return true;
            }
            match printer.as_mut() {
                Some(printer) => printer.print(text).is_ok(),
                None => {
                    print!("{}", text);
                    let _ = std::io::stdout().flush();
                    true
                }
            }
        };

        while let Some(event) = events.blocking_recv() {
            // A new prompt starts a new turn: whatever the last one left
            // unterminated must not run into the prompt header.
            let flush_before = matches!(event, AcpTurnEvent::PromptReceived { .. });
            // The turn is over: nothing will ever complete the pending line.
            let flush_after = matches!(
                event,
                AcpTurnEvent::TurnDone { .. } | AcpTurnEvent::TurnFailed { .. }
            );
            if flush_before && !deliver(lines.take()) {
                break;
            }
            let (rendered, _) = output::capture(|| {
                render(
                    event,
                    &mut markdown_buffer,
                    &mut progress_bars,
                    &mut thinking_header_shown,
                    debug,
                )
            });
            let ready = if flush_after {
                lines.push_all(&rendered)
            } else {
                lines.push(&rendered)
            };
            if !deliver(ready) {
                break;
            }
        }
        let _ = deliver(lines.take());
    });
}

/// Holds back the unterminated tail of the rendered stream until its line is
/// complete.
///
/// rustyline's external printer appends a newline to every message that does
/// not already end in one (`State::external_print`). A reply arrives as token
/// deltas, and the streaming buffer releases each delta as soon as it is safe
/// to render, so handing the deltas over one by one puts every few words on a
/// line of their own — however wide the terminal is. The tail is therefore kept
/// here and only complete lines go to the printer; the terminal does the
/// wrapping, at its real width.
#[derive(Default)]
struct LineCoalescer {
    pending: String,
}

impl LineCoalescer {
    /// Add `rendered`; return everything up to and including the last newline,
    /// keeping the unterminated rest back.
    fn push(&mut self, rendered: &str) -> String {
        self.pending.push_str(rendered);
        match self.pending.rfind('\n') {
            Some(end) => self.pending.drain(..=end).collect(),
            None => String::new(),
        }
    }

    /// Add `rendered` and return everything, terminated or not.
    fn push_all(&mut self, rendered: &str) -> String {
        self.pending.push_str(rendered);
        self.take()
    }

    /// Return and clear whatever is pending.
    fn take(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::LineCoalescer;

    #[test]
    fn deltas_without_a_newline_are_held_back() {
        let mut lines = LineCoalescer::default();
        assert_eq!(lines.push("Воз"), "");
        assert_eq!(lines.push("ражений"), "");
        assert_eq!(lines.push(" нет."), "");
    }

    #[test]
    fn a_completed_line_is_released_whole() {
        let mut lines = LineCoalescer::default();
        assert_eq!(lines.push("Воз"), "");
        assert_eq!(lines.push("ражений нет.\n"), "Возражений нет.\n");
        assert_eq!(lines.take(), "");
    }

    #[test]
    fn only_complete_lines_leave_and_the_tail_is_kept() {
        let mut lines = LineCoalescer::default();
        assert_eq!(lines.push("one\ntw"), "one\n");
        assert_eq!(lines.push("o\nthr"), "two\n");
        assert_eq!(lines.take(), "thr");
    }

    #[test]
    fn the_end_of_a_turn_releases_an_unterminated_tail() {
        let mut lines = LineCoalescer::default();
        assert_eq!(lines.push("last words"), "");
        assert_eq!(lines.push_all(""), "last words");
        assert_eq!(lines.take(), "");
    }
}

/// Render one mirror tick so an ACP-driven turn reads like a chat: the prompt as
/// a compact header, the reply through the same path as a user-typed turn.
/// Read-only — it never touches the agent or session state, so it cannot
/// interfere with the live ACP reply the bridge is already receiving.
///
/// The prompt header carries NO attribution. `session/prompt` says nothing about
/// who wrote it, and the same client that steers a session on a scheduler's
/// behalf also forwards what a human typed into a web terminal, so naming the
/// writer here would be a guess — and the guess reads as a fact to the operator
/// watching the pane. Attribution has to arrive with the prompt or not at all.
fn render(
    event: AcpTurnEvent,
    markdown_buffer: &mut streaming_buffer::MarkdownBuffer,
    progress_bars: &mut output::McpSpinners,
    thinking_header_shown: &mut bool,
    debug: bool,
) {
    match event {
        AcpTurnEvent::PromptReceived { text, .. } => {
            output::flush_markdown_buffer_current_theme(markdown_buffer);
            *thinking_header_shown = false;
            let trimmed = text.trim_end();
            if !trimmed.is_empty() {
                output::render_text("", None, false);
                output::render_text(&format!("▸ {trimmed}"), Some(Color::Cyan), false);
            }
        }
        AcpTurnEvent::StreamEvent(tick) => match *tick {
            AgentEvent::Message(message) => {
                output::render_message_streaming(
                    &message,
                    markdown_buffer,
                    thinking_header_shown,
                    debug,
                );
            }
            AgentEvent::McpNotification((extension_id, notification)) => {
                handle_mcp_notification(
                    &extension_id,
                    &notification,
                    progress_bars,
                    false,
                    true,
                    false,
                    debug,
                );
            }
            _ => {}
        },
        AcpTurnEvent::TurnDone { .. } => {
            output::flush_markdown_buffer_current_theme(markdown_buffer);
        }
        AcpTurnEvent::TurnFailed { error, .. } => {
            output::flush_markdown_buffer_current_theme(markdown_buffer);
            output::render_error(&format!("ACP turn ended: {error}"));
        }
    }
}
