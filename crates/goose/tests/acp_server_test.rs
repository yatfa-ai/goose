#![recursion_limit = "256"]
#[allow(dead_code)]
#[path = "acp_common_tests/mod.rs"]
mod common_tests;
use agent_client_protocol::schema::v1::{
    ListSessionsRequest, ListSessionsResponse, NewSessionRequest, SessionConfigKind,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionInfo,
    SetSessionConfigOptionRequest,
};
use agent_client_protocol::ErrorCode;
use common_tests::fixtures::server::AcpServerConnection;
use common_tests::fixtures::{run_test, Connection, OpenAiFixture, Session, TestConnectionConfig};
#[cfg(feature = "code-mode")]
use common_tests::run_prompt_codemode;
use common_tests::{
    run_close_session, run_config_mcp, run_config_option_mode_set, run_config_option_model_set,
    run_delete_session, run_fs_read_text_file_true, run_fs_write_text_file_false,
    run_fs_write_text_file_true, run_initialize_doesnt_hit_provider, run_list_sessions,
    run_load_mode, run_load_model, run_load_session_error, run_load_session_mcp,
    run_load_session_replays_image_attachment, run_mode_set, run_model_list, run_model_set,
    run_model_set_error_session_not_found, run_new_session_returns_initial_config,
    run_new_session_uses_current_config_mode, run_permission_persistence, run_prompt_basic,
    run_prompt_error, run_prompt_image, run_prompt_image_attachment, run_prompt_mcp,
    run_prompt_model_mismatch, run_prompt_skill, run_session_name_update_notification,
    run_shell_terminal_false, run_shell_terminal_true,
};
use goose::config::GooseMode;
use goose::conversation::message::{Message, MessageMetadata};
use goose::custom_requests::{GetSessionInfoRequest, GetSessionInfoResponse};
use goose::recipe::{Recipe, Settings};
use goose::recipe_deeplink;
use goose::session::{SessionManager, SessionType};
use std::path::Path;

tests_config_option_set_error!(AcpServerConnection);
tests_mode_set_error!(AcpServerConnection);

async fn seed_list_sessions(data_root: &Path, working_dir: &Path, count: usize) {
    let session_manager = SessionManager::new(data_root.to_path_buf());
    for index in 0..count {
        let session = session_manager
            .create_session(
                working_dir.to_path_buf(),
                format!("Seed session {index}"),
                SessionType::Acp,
                GooseMode::default(),
            )
            .await
            .unwrap();
        session_manager
            .add_message(&session.id, &Message::user().with_text("hello"))
            .await
            .unwrap();
    }
}

async fn seed_list_session_with_message(
    data_root: &Path,
    working_dir: &Path,
    name: &str,
    session_type: SessionType,
    message: &str,
) {
    let session_manager = SessionManager::new(data_root.to_path_buf());
    let session = session_manager
        .create_session(
            working_dir.to_path_buf(),
            name.to_string(),
            session_type,
            GooseMode::default(),
        )
        .await
        .unwrap();
    session_manager
        .add_message(&session.id, &Message::user().with_text(message))
        .await
        .unwrap();
}

async fn new_connection(data_root: &Path) -> AcpServerConnection {
    let openai = OpenAiFixture::new(
        vec![],
        <AcpServerConnection as Connection>::expected_session_id(),
    )
    .await;
    <AcpServerConnection as Connection>::new(
        TestConnectionConfig {
            data_root: data_root.to_path_buf(),
            ..Default::default()
        },
        openai,
    )
    .await
}

async fn list_sessions_request(
    conn: &AcpServerConnection,
    request: ListSessionsRequest,
) -> anyhow::Result<ListSessionsResponse> {
    conn.cx()
        .send_request(request)
        .block_task()
        .await
        .map_err(Into::into)
}

async fn get_session_info_request(
    conn: &AcpServerConnection,
    request: GetSessionInfoRequest,
) -> anyhow::Result<GetSessionInfoResponse> {
    conn.cx()
        .send_request(request)
        .block_task()
        .await
        .map_err(Into::into)
}

fn assert_invalid_params(error: anyhow::Error) {
    let acp_error = error.downcast::<agent_client_protocol::Error>().unwrap();
    assert_eq!(acp_error.code, ErrorCode::InvalidParams);
}

fn include_last_message_snippet_meta(
    value: serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    let mut goose = serde_json::Map::new();
    goose.insert("includeLastMessageSnippet".to_string(), value);

    let mut meta = serde_json::Map::new();
    meta.insert("goose".to_string(), serde_json::Value::Object(goose));
    meta
}

fn last_message_snippet(session: &SessionInfo) -> Option<&str> {
    session
        .meta
        .as_ref()
        .and_then(|meta| meta.get("lastMessageSnippet"))
        .and_then(serde_json::Value::as_str)
}

#[test]
fn test_config_mcp() {
    run_test(async { run_config_mcp::<AcpServerConnection>().await });
}

#[test]
fn test_config_option_mode_set() {
    run_test(async { run_config_option_mode_set::<AcpServerConnection>().await });
}

#[test]
fn test_list_sessions() {
    run_test(async { run_list_sessions::<AcpServerConnection>().await });
}

#[test]
fn test_list_sessions_emits_computed_snippet() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/acp-session-list-snippet");
        let session_manager = SessionManager::new(data_root.path().to_path_buf());
        let session = session_manager
            .create_session(
                cwd.to_path_buf(),
                "Live subtitle".to_string(),
                SessionType::Acp,
                GooseMode::default(),
            )
            .await
            .unwrap();
        session_manager
            .add_message(
                &session.id,
                &Message::user().with_text("**raw** _markdown_ subtitle"),
            )
            .await
            .unwrap();
        session_manager
            .add_message(
                &session.id,
                &Message::assistant()
                    .with_text("hidden newer text")
                    .with_metadata(MessageMetadata::agent_only()),
            )
            .await
            .unwrap();

        let conn = new_connection(data_root.path()).await;
        let response = list_sessions_request(
            &conn,
            ListSessionsRequest::new()
                .meta(include_last_message_snippet_meta(serde_json::Value::Null)),
        )
        .await
        .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(last_message_snippet(&response.sessions[0]), None);

        let response = list_sessions_request(
            &conn,
            ListSessionsRequest::new().meta(include_last_message_snippet_meta(
                serde_json::Value::Bool(false),
            )),
        )
        .await
        .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(last_message_snippet(&response.sessions[0]), None);

        let response = list_sessions_request(
            &conn,
            ListSessionsRequest::new().meta(include_last_message_snippet_meta(
                serde_json::Value::Bool(true),
            )),
        )
        .await
        .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(
            last_message_snippet(&response.sessions[0]),
            Some("**raw** _markdown_ subtitle")
        );
    });
}

#[test]
fn test_list_sessions_pagination() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        seed_list_sessions(data_root.path(), Path::new("/tmp/acp-session-list"), 51).await;
        let conn = new_connection(data_root.path()).await;

        let first = list_sessions_request(&conn, ListSessionsRequest::new())
            .await
            .unwrap();
        assert_eq!(first.sessions.len(), 50);
        assert!(first
            .sessions
            .iter()
            .all(|session| last_message_snippet(session).is_none()));

        let second = list_sessions_request(
            &conn,
            ListSessionsRequest::new()
                .cursor(first.next_cursor.clone().unwrap())
                .meta(include_last_message_snippet_meta(serde_json::Value::Bool(
                    true,
                ))),
        )
        .await
        .unwrap();
        assert_eq!(second.sessions.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_eq!(last_message_snippet(&second.sessions[0]), Some("hello"));

        let second_id = &second.sessions[0].session_id;
        assert!(first
            .sessions
            .iter()
            .all(|session| session.session_id != *second_id));
    });
}

#[test]
fn test_list_sessions_query_filters_results() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/acp-session-list");
        seed_list_session_with_message(
            data_root.path(),
            cwd,
            "Postgres session",
            SessionType::Acp,
            "Discuss Postgres migrations",
        )
        .await;
        seed_list_session_with_message(
            data_root.path(),
            cwd,
            "Mobile session",
            SessionType::Acp,
            "Plan the mobile release",
        )
        .await;
        let conn = new_connection(data_root.path()).await;

        let mut meta = serde_json::Map::new();
        meta.insert(
            "query".to_string(),
            serde_json::Value::String("postgres".to_string()),
        );
        let response = list_sessions_request(&conn, ListSessionsRequest::new().meta(meta))
            .await
            .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(
            response.sessions[0].title.as_deref(),
            Some("Postgres session")
        );
        assert!(response.next_cursor.is_none());
    });
}

#[test]
fn test_list_sessions_types_override_filters_results() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/acp-session-list");
        seed_list_session_with_message(
            data_root.path(),
            cwd,
            "ACP session",
            SessionType::Acp,
            "ACP message",
        )
        .await;
        seed_list_session_with_message(
            data_root.path(),
            cwd,
            "User session",
            SessionType::User,
            "User message",
        )
        .await;
        let conn = new_connection(data_root.path()).await;

        let mut meta = serde_json::Map::new();
        meta.insert(
            "types".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String("user".to_string())]),
        );
        let response = list_sessions_request(&conn, ListSessionsRequest::new().meta(meta))
            .await
            .unwrap();

        assert_eq!(response.sessions.len(), 1);
        assert_eq!(response.sessions[0].title.as_deref(), Some("User session"));
        assert!(response.next_cursor.is_none());
    });
}

#[test]
fn test_list_sessions_types_rejects_internal_session_types() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let conn = new_connection(data_root.path()).await;

        for session_type in ["hidden", "sub_agent"] {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "types".to_string(),
                serde_json::Value::Array(vec![serde_json::Value::String(session_type.to_string())]),
            );

            let error = list_sessions_request(&conn, ListSessionsRequest::new().meta(meta))
                .await
                .unwrap_err();
            assert_invalid_params(error);
        }
    });
}

#[test]
fn test_list_sessions_invalid_params() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let other_cwd = tempfile::tempdir().unwrap();
        seed_list_sessions(data_root.path(), cwd.path(), 51).await;
        let conn = new_connection(data_root.path()).await;

        let error =
            list_sessions_request(&conn, ListSessionsRequest::new().cursor("*".to_string()))
                .await
                .unwrap_err();
        assert_invalid_params(error);

        let error = list_sessions_request(
            &conn,
            ListSessionsRequest::new().cwd(std::path::PathBuf::from("relative/path")),
        )
        .await
        .unwrap_err();
        assert_invalid_params(error);

        let first = list_sessions_request(&conn, ListSessionsRequest::new().cwd(cwd.path()))
            .await
            .unwrap();

        let error = list_sessions_request(
            &conn,
            ListSessionsRequest::new()
                .cwd(other_cwd.path())
                .cursor(first.next_cursor.unwrap()),
        )
        .await
        .unwrap_err();
        assert_invalid_params(error);

        let error = list_sessions_request(
            &conn,
            ListSessionsRequest::new().meta(include_last_message_snippet_meta(
                serde_json::Value::String("true".to_string()),
            )),
        )
        .await
        .unwrap_err();
        assert_invalid_params(error);
    });
}

#[test]
fn test_get_session_info() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/acp-session-info");
        let session_manager = SessionManager::new(data_root.path().to_path_buf());
        let session = session_manager
            .create_session(
                cwd.to_path_buf(),
                "Session info".to_string(),
                SessionType::Acp,
                GooseMode::default(),
            )
            .await
            .unwrap();
        session_manager
            .add_message(&session.id, &Message::user().with_text("hello"))
            .await
            .unwrap();
        let conn = new_connection(data_root.path()).await;

        let response = get_session_info_request(
            &conn,
            GetSessionInfoRequest {
                session_id: session.id.clone(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            response.session.session_id,
            agent_client_protocol::schema::v1::SessionId::new(session.id)
        );
        assert_eq!(response.session.cwd, cwd.to_path_buf());
        assert_eq!(response.session.title.as_deref(), Some("Session info"));
        assert!(response.session.updated_at.is_some());

        let meta = response
            .session
            .meta
            .expect("session info should include meta");
        assert!(meta.get("createdAt").and_then(|v| v.as_str()).is_some());
        assert_eq!(meta.get("messageCount"), Some(&serde_json::json!(1)));
        assert_eq!(meta.get("userSetName"), Some(&serde_json::json!(false)));
        assert_eq!(meta.get("sessionType"), Some(&serde_json::json!("acp")));
        assert_eq!(meta.get("hasRecipe"), Some(&serde_json::json!(false)));
    });
}

#[test]
fn test_session_name_update_notification() {
    run_test(async { run_session_name_update_notification::<AcpServerConnection>().await });
}

#[test]
fn test_close_session() {
    run_test(async { run_close_session::<AcpServerConnection>().await });
}

#[test]
fn test_config_option_model_set() {
    run_test(async { run_config_option_model_set::<AcpServerConnection>().await });
}

#[test]
fn test_config_option_thinking_effort_set() {
    run_test(async {
        let openai = OpenAiFixture::new(
            vec![],
            <AcpServerConnection as Connection>::expected_session_id(),
        )
        .await;
        let mut conn = <AcpServerConnection as Connection>::new(
            TestConnectionConfig {
                current_model: "claude-sonnet-4".to_string(),
                ..Default::default()
            },
            openai,
        )
        .await;
        let data = conn.new_session().await.unwrap();

        let response = conn
            .cx()
            .send_request(SetSessionConfigOptionRequest::new(
                data.session.session_id().clone(),
                "thinking_effort".to_string(),
                SessionConfigOptionValue::value_id("high".to_string()),
            ))
            .block_task()
            .await
            .unwrap();

        let option = response
            .config_options
            .iter()
            .find(|option| option.id.0.as_ref() == "thinking_effort")
            .expect("thinking_effort option");
        assert_eq!(
            option.category,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        );
        let select = match &option.kind {
            SessionConfigKind::Select(select) => select,
            _ => panic!("thinking_effort should be a select option"),
        };

        assert_eq!(select.current_value.0.as_ref(), "high");
    });
}

#[test]
fn test_config_option_non_value_id_returns_error_and_keeps_connection() {
    run_test(async {
        let openai = OpenAiFixture::new(
            vec![],
            <AcpServerConnection as Connection>::expected_session_id(),
        )
        .await;
        let mut conn =
            <AcpServerConnection as Connection>::new(TestConnectionConfig::default(), openai).await;
        let data = conn.new_session().await.unwrap();

        let err = conn
            .cx()
            .send_request(SetSessionConfigOptionRequest::new(
                data.session.session_id().clone(),
                "mode".to_string(),
                SessionConfigOptionValue::boolean(true),
            ))
            .block_task()
            .await
            .unwrap_err();
        assert_eq!(
            err,
            agent_client_protocol::Error::invalid_params().data("Expected a value ID")
        );

        conn.cx()
            .send_request(ListSessionsRequest::new())
            .block_task()
            .await
            .expect("connection should survive an invalid set_config_option");
    });
}

#[test]
fn test_delete_session() {
    run_test(async { run_delete_session::<AcpServerConnection>().await });
}

#[test]
fn test_fs_read_text_file_true() {
    run_test(async { run_fs_read_text_file_true::<AcpServerConnection>().await });
}

#[test]
fn test_fs_write_text_file_false() {
    run_test(async { run_fs_write_text_file_false::<AcpServerConnection>().await });
}

#[test]
fn test_fs_write_text_file_true() {
    run_test(async { run_fs_write_text_file_true::<AcpServerConnection>().await });
}

#[test]
fn test_initialize_doesnt_hit_provider() {
    run_test(async { run_initialize_doesnt_hit_provider::<AcpServerConnection>().await });
}

#[test]
fn test_load_mode() {
    run_test(async { run_load_mode::<AcpServerConnection>().await });
}

#[test]
fn test_load_model() {
    run_test(async { run_load_model::<AcpServerConnection>().await });
}

#[test]
fn test_load_session_error_session_not_found() {
    run_test(async { run_load_session_error::<AcpServerConnection>().await });
}

#[test]
fn test_load_session_mcp() {
    run_test(async { run_load_session_mcp::<AcpServerConnection>().await });
}

#[test]
fn test_load_session_replays_image_attachment() {
    run_test(async { run_load_session_replays_image_attachment::<AcpServerConnection>().await });
}

#[test]
fn test_mode_set() {
    run_test(async { run_mode_set::<AcpServerConnection>().await });
}

#[test]
fn test_model_list() {
    run_test(async { run_model_list::<AcpServerConnection>().await });
}

#[test]
fn test_new_session_returns_initial_config() {
    run_test(async { run_new_session_returns_initial_config::<AcpServerConnection>().await });
}

#[test]
fn test_new_session_uses_current_config_mode() {
    run_test(async { run_new_session_uses_current_config_mode::<AcpServerConnection>().await });
}

#[test]
fn test_new_session_honors_recipe_model_without_recipe_provider() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let conn = new_connection(data_root.path()).await;
        let work_dir = tempfile::tempdir().unwrap();
        let recipe_model = "gpt-4.1";
        let recipe = Recipe::builder()
            .title("Recipe model")
            .description("A recipe that only overrides the model")
            .instructions("Use the requested model")
            .settings(Settings {
                goose_provider: None,
                goose_model: Some(recipe_model.to_string()),
                temperature: None,
                max_turns: None,
            })
            .build()
            .unwrap();
        let mut meta = serde_json::Map::new();
        meta.insert(
            "recipeDeeplink".to_string(),
            serde_json::Value::String(recipe_deeplink::encode(&recipe).unwrap()),
        );

        let response = conn
            .cx()
            .send_request(NewSessionRequest::new(work_dir.path()).meta(meta))
            .block_task()
            .await
            .unwrap();
        let session_info = get_session_info_request(
            &conn,
            GetSessionInfoRequest {
                session_id: response.session_id.0.to_string(),
            },
        )
        .await
        .unwrap();
        let meta = session_info
            .session
            .meta
            .expect("session info should include meta");

        assert_eq!(
            meta.get("modelId").and_then(|v| v.as_str()),
            Some(recipe_model)
        );
        assert_eq!(
            meta.get("providerId").and_then(|v| v.as_str()),
            Some("openai")
        );
    });
}

#[test]
fn test_new_session_cleans_up_when_config_fails() {
    run_test(async {
        let data_root = tempfile::tempdir().unwrap();
        let conn = new_connection(data_root.path()).await;
        let work_dir = tempfile::tempdir().unwrap();
        let mut meta = serde_json::Map::new();
        meta.insert(
            "enabledExtensions".to_string(),
            serde_json::Value::String("invalid".to_string()),
        );

        let error: anyhow::Error = conn
            .cx()
            .send_request(NewSessionRequest::new(work_dir.path()).meta(meta))
            .block_task()
            .await
            .unwrap_err()
            .into();

        assert_invalid_params(error);

        let sessions = SessionManager::new(data_root.path().to_path_buf())
            .list_all_sessions()
            .await
            .unwrap();
        assert!(sessions.is_empty());
    });
}

#[test]
fn test_model_set() {
    run_test(async { run_model_set::<AcpServerConnection>().await });
}

#[test]
fn test_model_set_error_session_not_found() {
    run_test(async { run_model_set_error_session_not_found::<AcpServerConnection>().await });
}

#[test]
fn test_permission_persistence() {
    run_test(async { run_permission_persistence::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_basic() {
    run_test(async { run_prompt_basic::<AcpServerConnection>().await });
}

#[test]
#[cfg(feature = "code-mode")]
fn test_prompt_codemode() {
    run_test(async { run_prompt_codemode::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_error_session_not_found() {
    run_test(async { run_prompt_error::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_image() {
    run_test(async { run_prompt_image::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_image_attachment() {
    run_test(async { run_prompt_image_attachment::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_mcp() {
    run_test(async { run_prompt_mcp::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_model_mismatch() {
    run_test(async { run_prompt_model_mismatch::<AcpServerConnection>().await });
}

#[test]
fn test_prompt_skill() {
    run_test(async { run_prompt_skill::<AcpServerConnection>().await });
}

#[test]
fn test_shell_terminal_false() {
    run_test(async { run_shell_terminal_false::<AcpServerConnection>().await });
}

#[test]
fn test_shell_terminal_true() {
    run_test(async { run_shell_terminal_true::<AcpServerConnection>().await });
}

// ---------------------------------------------------------------------------
// Pane-mirror tap (fork-only; see ACP-ATTACH.yatfa.md "4. Rendering another
// client's turn in the pane").
//
// `on_prompt` duplicates each turn onto `event_tap` so an interactive `goose
// run` renders dispatcher-driven ACP turns instead of showing a frozen pane to
// an operator attached over tmux. Every other ACP test leaves the tap `None`,
// so these are the only tests in which `tap_turn_event`'s enabled branch runs.
// ---------------------------------------------------------------------------

use common_tests::fixtures::PermissionDecision;
use goose::acp::AcpTurnEvent;

/// Mirrors the private `TURN_CONTEXT_OPEN` in `acp_common_tests`: the provider
/// sees the turn-context suffix, while the tap sees the raw prompt blocks.
const TAP_TURN_CONTEXT_OPEN: &str = r#"\n<turn-context>"#;

fn drain_tap(rx: &mut tokio::sync::mpsc::Receiver<AcpTurnEvent>) -> Vec<AcpTurnEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// Connection whose provider answers the one `what is 1+1` exchange with "2".
async fn tap_connection(event_tap: tokio::sync::mpsc::Sender<AcpTurnEvent>) -> AcpServerConnection {
    let expected_session_id = <AcpServerConnection as Connection>::expected_session_id();
    let openai = OpenAiFixture::new(
        vec![(
            format!("what is 1+1{TAP_TURN_CONTEXT_OPEN}"),
            include_str!("acp_test_data/openai_basic.txt"),
        )],
        expected_session_id,
    )
    .await;
    <AcpServerConnection as Connection>::new(
        TestConnectionConfig {
            event_tap: Some(event_tap),
            ..Default::default()
        },
        openai,
    )
    .await
}

/// Success criterion 2 — the observed mirror of one dispatcher turn is
/// `PromptReceived` (carrying the prompt text) -> >=1 `StreamEvent` ->
/// `TurnDone`. Covers three of the four `tap_turn_event` call sites.
#[test]
fn test_prompt_taps_turn_events_in_order() {
    run_test(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AcpTurnEvent>(256);
        let mut conn = tap_connection(tx).await;
        let mut data = conn.new_session().await.unwrap();

        let output = data
            .session
            .prompt("what is 1+1", PermissionDecision::Cancel)
            .await
            .unwrap();
        assert_eq!(output.text, "2");

        let events = drain_tap(&mut rx);
        assert!(
            events.len() >= 3,
            "expected PromptReceived + >=1 StreamEvent + TurnDone, got {events:?}"
        );

        let turn_session_id = match &events[0] {
            AcpTurnEvent::PromptReceived { session_id, text } => {
                assert_eq!(
                    text, "what is 1+1",
                    "PromptReceived must carry the dispatcher's prompt text so the TUI can \
                     render the user-side bubble of the turn"
                );
                session_id.clone()
            }
            other => panic!("first tapped event must be PromptReceived, got {other:?}"),
        };

        // The tap lives inside the reply-stream loop, so a real turn mirrors at
        // least one tick. Hoisting it out of the loop drops this to zero.
        let stream_ticks = events
            .iter()
            .filter(|event| matches!(event, AcpTurnEvent::StreamEvent(_)))
            .count();
        assert!(
            stream_ticks >= 1,
            "expected >=1 StreamEvent tapped from the reply stream, got {events:?}"
        );

        match events.last().expect("non-empty") {
            AcpTurnEvent::TurnDone { session_id } => assert_eq!(
                session_id, &turn_session_id,
                "TurnDone must close the same turn PromptReceived opened"
            ),
            other => panic!("last tapped event must be TurnDone, got {other:?}"),
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AcpTurnEvent::TurnFailed { .. })),
            "a turn that completed normally must not tap TurnFailed: {events:?}"
        );
    });
}

/// Success criterion 3 — the failure path taps `TurnFailed`, not `TurnDone`.
/// A 402 from the provider becomes `ProviderError::CreditsExhausted`, which the
/// agent emits as a system notification and `on_prompt` recognises as a prompt
/// error — so the turn ends on the `stream_error` branch with no timing race.
#[test]
fn test_prompt_error_taps_turn_failed_not_turn_done() {
    run_test(async {
        let openai = OpenAiFixture::failing_with_status(402).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AcpTurnEvent>(256);
        let mut conn = <AcpServerConnection as Connection>::new(
            TestConnectionConfig {
                event_tap: Some(tx),
                ..Default::default()
            },
            openai,
        )
        .await;
        let mut data = conn.new_session().await.unwrap();

        data.session
            .prompt("what is 1+1", PermissionDecision::Cancel)
            .await
            .expect_err("a turn whose provider is out of credits must fail");

        let events = drain_tap(&mut rx);
        assert!(
            matches!(events.first(), Some(AcpTurnEvent::PromptReceived { .. })),
            "the turn should still open with PromptReceived, got {events:?}"
        );
        let failure = events
            .iter()
            .find_map(|event| match event {
                AcpTurnEvent::TurnFailed { error, .. } => Some(error),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a failed turn must tap TurnFailed, got {events:?}"));
        assert!(
            !failure.is_empty(),
            "TurnFailed must carry a non-empty reason for the pane to render"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AcpTurnEvent::TurnDone { .. })),
            "a failed turn must not tap TurnDone: {events:?}"
        );
    });
}

/// Success criterion 4 — the load-bearing contract at `tap_turn_event`: "a full
/// channel ... drops the event without affecting the ACP reply path". The tap
/// fires on *every* reply tick into a channel bounded at 64, so a full channel
/// is an ordinary operating condition. If `try_send` ever became `send().await`,
/// the dispatcher's primary control path would deadlock here.
#[test]
fn test_full_tap_channel_drops_events_without_blocking_reply() {
    run_test(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AcpTurnEvent>(1);
        // Pre-fill to capacity, and hold `rx` without draining so it stays full
        // for the whole turn. Every tap during the turn must now fail to send.
        tx.try_send(AcpTurnEvent::TurnDone {
            session_id: "pre-filled".to_string(),
        })
        .expect("first send fills the buffer");
        assert!(
            tx.try_send(AcpTurnEvent::TurnDone {
                session_id: "overflow".to_string(),
            })
            .is_err(),
            "channel must be at capacity before the turn starts"
        );

        let mut conn = tap_connection(tx).await;
        let mut data = conn.new_session().await.unwrap();

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            data.session
                .prompt("what is 1+1", PermissionDecision::Cancel),
        )
        .await
        .expect("a full tap channel must never block the ACP reply path")
        .expect("the ACP reply must succeed even though every tap was dropped");
        assert_eq!(
            output.text, "2",
            "the reply the dispatcher receives is unaffected by the dropped mirror"
        );

        // Best-effort means dropped, not queued: nothing from the turn landed
        // behind the pre-filled event.
        let events = drain_tap(&mut rx);
        assert_eq!(
            events.len(),
            1,
            "turn events must be discarded when the channel is full, got {events:?}"
        );
        assert!(
            matches!(&events[0], AcpTurnEvent::TurnDone { session_id } if session_id == "pre-filled"),
            "only the pre-filled event should remain, got {events:?}"
        );
    });
}

/// Success criterion 5 — a detached TUI (receiver dropped) is the other half of
/// the same best-effort contract: `try_send` returns `Err(Closed)` and the
/// event is discarded rather than unwrapped.
#[test]
fn test_closed_tap_receiver_does_not_affect_reply() {
    run_test(async {
        let (tx, rx) = tokio::sync::mpsc::channel::<AcpTurnEvent>(64);
        drop(rx);
        assert!(
            tx.try_send(AcpTurnEvent::TurnDone {
                session_id: "detached".to_string(),
            })
            .is_err(),
            "dropping the receiver must close the channel before the turn starts"
        );

        let mut conn = tap_connection(tx).await;
        let mut data = conn.new_session().await.unwrap();

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            data.session
                .prompt("what is 1+1", PermissionDecision::Cancel),
        )
        .await
        .expect("a closed tap channel must never block the ACP reply path")
        .expect("the ACP reply must succeed with no TUI attached");
        assert_eq!(output.text, "2");
    });
}
