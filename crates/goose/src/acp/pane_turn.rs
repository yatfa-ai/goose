//! Turns typed in the pane, published to the socket.
//!
//! The mirror in [`crate::acp::turn_event`] carries an ACP-driven turn *into*
//! the terminal. This is the other direction: a turn a human typed in an
//! interactive `goose run` is invisible to every attached ACP client, because
//! notifications are sent through the connection that is serving a request and
//! a locally-typed turn is serving none (upstream #10130). An attached client —
//! a monitor, a transcript builder, a browser rendering the session — therefore
//! sees a session that appears to stop whenever the human takes over.
//!
//! `goose run` publishes its own reply stream here; each live ACP connection
//! subscribes and forwards what it sees to its client.
//!
//! DELIBERATELY NOTIFICATION-ONLY. The translation below produces
//! `SessionUpdate`s and nothing else: it registers no pending permission, mints
//! no run id, and touches no session state. The pane owns that turn — it is
//! already prompting its human for approvals and already persisting the
//! conversation — so a forwarder that did any of it would be a second owner
//! racing the first.

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCallId, ToolCallUpdate,
};
use rmcp::model::Role;

use crate::acp::server::tool_calls::conversion::{
    build_initial_tool_call_with_message_meta, tool_call_update_fields_from_response,
};
use crate::agents::AgentEvent;
use crate::conversation::message::{Message, MessageContent};

/// One tick of a pane-driven turn, addressed to the session it belongs to.
#[derive(Clone, Debug)]
pub struct PaneTurnEvent {
    pub session_id: String,
    pub event: Arc<AgentEvent>,
}

/// Channel the interactive session publishes to, and every live ACP connection
/// subscribes to.
///
/// A broadcast rather than a queue: there are N connections and each needs the
/// same event, and a slow or dead subscriber must not hold the pane's render
/// loop. A subscriber that falls behind loses the oldest events and learns it
/// did, which is the honest failure for a live view — the stored conversation
/// remains the complete record.
pub type PaneTurnPublisher = tokio::sync::broadcast::Sender<PaneTurnEvent>;

/// Translate one agent event into the updates a client should see.
///
/// Returns an empty vector for events that carry nothing a renderer can use, so
/// a caller can treat "nothing to send" as ordinary.
pub fn pane_turn_updates(
    event: &AgentEvent,
    requests: &mut std::collections::HashMap<String, crate::conversation::message::ToolRequest>,
) -> Vec<SessionUpdate> {
    match event {
        AgentEvent::Message(message) => message_updates(message, requests),
        // A tool's live output, so a client watching a pane-driven shell command
        // sees it fill in rather than appearing whole at the end. Same converter
        // the socket-driven path uses.
        AgentEvent::McpNotification((request_id, notification)) => {
            match crate::acp::server::tool_notifications::tool_notification_update(
                request_id.clone(),
                notification.clone(),
            ) {
                Some(update) => vec![SessionUpdate::ToolCallUpdate(update)],
                None => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// `requests` carries the turn's tool calls forward: a response's update is
/// built against the request that opened it, exactly as the stored-conversation
/// replay does.
fn message_updates(
    message: &Message,
    requests: &mut std::collections::HashMap<String, crate::conversation::message::ToolRequest>,
) -> Vec<SessionUpdate> {
    let mut updates = Vec::new();
    for content in &message.content {
        match content {
            MessageContent::Text(text) if !text.text.trim().is_empty() => {
                let chunk =
                    ContentChunk::new(ContentBlock::Text(TextContent::new(text.text.clone())));
                updates.push(match message.role {
                    Role::User => SessionUpdate::UserMessageChunk(chunk),
                    _ => SessionUpdate::AgentMessageChunk(chunk),
                });
            }
            MessageContent::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                updates.push(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(thinking.thinking.clone())),
                )));
            }
            MessageContent::ToolRequest(request) => {
                requests.insert(request.id.clone(), request.clone());
                updates.push(SessionUpdate::ToolCall(
                    build_initial_tool_call_with_message_meta(request, message, true),
                ));
            }
            MessageContent::ToolResponse(response) => {
                let fields = tool_call_update_fields_from_response(
                    response,
                    requests.get(&response.id),
                    true,
                );
                updates.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    ToolCallId::new(response.id.clone()),
                    fields,
                )));
            }
            _ => {}
        }
    }
    updates
}

/// Build the notification for one update.
pub fn pane_turn_notification(session_id: &str, update: SessionUpdate) -> SessionNotification {
    SessionNotification::new(SessionId::new(session_id.to_string()), update)
}
