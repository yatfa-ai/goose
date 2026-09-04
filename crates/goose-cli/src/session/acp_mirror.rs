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

        while let Some(event) = events.blocking_recv() {
            let (rendered, _) = output::capture(|| {
                render(
                    event,
                    &mut markdown_buffer,
                    &mut progress_bars,
                    &mut thinking_header_shown,
                    debug,
                )
            });
            if rendered.is_empty() {
                continue;
            }
            match printer.as_mut() {
                Some(printer) => {
                    if printer.print(rendered).is_err() {
                        break;
                    }
                }
                None => {
                    print!("{}", rendered);
                    let _ = std::io::stdout().flush();
                }
            }
        }
    });
}

/// Render one mirror tick so a dispatcher turn reads like a chat: the prompt as
/// a compact header, the reply through the same path as a user-typed turn.
/// Read-only — it never touches the agent or session state, so it cannot
/// interfere with the live ACP reply the bridge is already receiving.
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
                output::render_text(&format!("dispatcher ▸ {trimmed}"), Some(Color::Cyan), false);
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
