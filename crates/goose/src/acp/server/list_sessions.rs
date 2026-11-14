use super::{
    build_session_info, is_acp_visible_session_type, meta_string, meta_tags, AcpConnection,
    ResultExt, ACP_VISIBLE_SESSION_TYPES,
};
use crate::session::session_manager::{
    SessionListCursor, SessionListFilters, SessionListPageQuery, SessionType,
};
use agent_client_protocol::schema::v1::{
    ListSessionsRequest, ListSessionsResponse, Meta, SessionInfo,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SESSION_LIST_PAGE_SIZE: usize = 50;

#[derive(Debug, Serialize, Deserialize)]
struct SessionListCursorToken {
    #[serde(alias = "updated_at")]
    sort_at: chrono::DateTime<chrono::Utc>,
    // Goose stores timestamps with second precision in common write paths, so the
    // cursor needs the full (sort_at, id) sort key to avoid skipping tied rows.
    session_id: String,
    filter_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionListCursorFilters<'a> {
    cwd: Option<String>,
    session_types: Vec<String>,
    keyword: Option<String>,
    // Omitted when empty so cursors issued before tags existed stay valid.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tags: &'a [(String, String)],
    only_sessions_with_messages: bool,
}

fn invalid_session_list_cursor(message: &'static str) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message)
}

fn session_keyword_from_meta(
    meta: Option<&Meta>,
) -> Result<Option<String>, agent_client_protocol::Error> {
    Ok(meta_string(meta, "query")?
        .map(|keyword| keyword.trim().to_string())
        .filter(|keyword| !keyword.is_empty()))
}

fn session_types_from_meta(
    meta: Option<&Meta>,
) -> Result<Vec<SessionType>, agent_client_protocol::Error> {
    let Some(value) = meta.and_then(|meta| meta.get("types")) else {
        return Ok(ACP_VISIBLE_SESSION_TYPES.to_vec());
    };
    if value.is_null() {
        return Ok(ACP_VISIBLE_SESSION_TYPES.to_vec());
    }

    let session_types =
        serde_json::from_value::<Vec<SessionType>>(value.clone()).map_err(|_| {
            agent_client_protocol::Error::invalid_params()
                .data("types must be an array of session type strings")
        })?;
    if session_types.is_empty() {
        Ok(ACP_VISIBLE_SESSION_TYPES.to_vec())
    } else {
        if session_types
            .iter()
            .any(|session_type| !is_acp_visible_session_type(session_type))
        {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("types may only include user, scheduled, or acp"));
        }
        Ok(session_types)
    }
}

fn include_last_message_snippet_from_meta(
    meta: Option<&Meta>,
) -> Result<bool, agent_client_protocol::Error> {
    let Some(value) = meta.and_then(|meta| meta.get("goose")) else {
        return Ok(false);
    };
    if value.is_null() {
        return Ok(false);
    }

    let Some(goose_meta) = value.as_object() else {
        return Err(agent_client_protocol::Error::invalid_params().data("goose must be an object"));
    };
    let Some(value) = goose_meta.get("includeLastMessageSnippet") else {
        return Ok(false);
    };
    if value.is_null() {
        return Ok(false);
    }

    value.as_bool().ok_or_else(|| {
        agent_client_protocol::Error::invalid_params()
            .data("goose.includeLastMessageSnippet must be a boolean")
    })
}

// bind cursors to the effective filters so they cannot be reused for a different list.
fn session_list_filter_hash(
    cwd: Option<&std::path::Path>,
    session_types: &[SessionType],
    keyword: Option<&str>,
    tags: &[(String, String)],
) -> Result<String, agent_client_protocol::Error> {
    let mut session_type_names = session_types
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    session_type_names.sort();
    let filters = SessionListCursorFilters {
        cwd: cwd.map(|path| path.to_string_lossy().to_string()),
        session_types: session_type_names,
        keyword: keyword.map(ToString::to_string),
        tags,
        only_sessions_with_messages: true,
    };
    let bytes =
        serde_json::to_vec(&filters).internal_err_ctx("Failed to encode session list filters")?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

fn decode_session_list_cursor(
    cursor: Option<&str>,
    cwd: Option<&std::path::Path>,
    session_types: &[SessionType],
    keyword: Option<&str>,
    tags: &[(String, String)],
) -> Result<Option<SessionListCursor>, agent_client_protocol::Error> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };

    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| invalid_session_list_cursor("malformed session list cursor"))?;
    let token: SessionListCursorToken = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_session_list_cursor("malformed session list cursor"))?;

    if token.session_id.is_empty() || token.filter_hash.is_empty() {
        return Err(invalid_session_list_cursor("malformed session list cursor"));
    }

    let expected_filter_hash = session_list_filter_hash(cwd, session_types, keyword, tags)?;
    if token.filter_hash != expected_filter_hash {
        return Err(invalid_session_list_cursor(
            "session list cursor does not match filters",
        ));
    }

    Ok(Some(SessionListCursor {
        sort_at: token.sort_at,
        session_id: token.session_id,
    }))
}

fn encode_session_list_cursor(
    cursor: &SessionListCursor,
    cwd: Option<&std::path::Path>,
    session_types: &[SessionType],
    keyword: Option<&str>,
    tags: &[(String, String)],
) -> Result<String, agent_client_protocol::Error> {
    let token = SessionListCursorToken {
        sort_at: cursor.sort_at,
        session_id: cursor.session_id.clone(),
        filter_hash: session_list_filter_hash(cwd, session_types, keyword, tags)?,
    };
    let bytes =
        serde_json::to_vec(&token).internal_err_ctx("Failed to encode session list cursor")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl AcpConnection {
    pub(super) async fn on_list_sessions(
        &self,
        req: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, agent_client_protocol::Error> {
        if let Some(cwd) = req.cwd.as_deref() {
            if !cwd.is_absolute() {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("cwd must be an absolute path"));
            }
        }

        let cwd = req.cwd.as_deref();
        let keyword = session_keyword_from_meta(req.meta.as_ref())?;
        let session_types = session_types_from_meta(req.meta.as_ref())?;
        let tags = meta_tags(req.meta.as_ref())?;
        let include_last_message_snippet =
            include_last_message_snippet_from_meta(req.meta.as_ref())?;
        let cursor = decode_session_list_cursor(
            req.cursor.as_deref(),
            cwd,
            &session_types,
            keyword.as_deref(),
            &tags,
        )?;

        // ACP clients see their own (Acp) sessions plus legacy User/Scheduled ones.
        let page = self
            .services
            .session_manager
            .list_sessions_paged(SessionListPageQuery {
                filters: SessionListFilters {
                    types: Some(&session_types),
                    working_dir: cwd,
                    keyword: keyword.as_deref(),
                    tags: &tags,
                    only_sessions_with_messages: true,
                },
                cursor: cursor.as_ref(),
                page_size: SESSION_LIST_PAGE_SIZE,
                include_last_message_snippet,
            })
            .await
            .internal_err()?;

        let mut session_infos: Vec<SessionInfo> =
            page.sessions.into_iter().map(build_session_info).collect();
        self.mark_live_session(
            &mut session_infos,
            req.cursor.is_none() && keyword.is_none(),
            cwd,
            &session_types,
        )
        .await;
        let next_cursor = page
            .next_cursor
            .as_ref()
            .map(|cursor| {
                encode_session_list_cursor(cursor, cwd, &session_types, keyword.as_deref(), &tags)
            })
            .transpose()?;
        Ok(ListSessionsResponse::new(session_infos).next_cursor(next_cursor))
    }

    /// Point a client at the session this process is actually driving.
    ///
    /// Two things make the live session hard to find. It is hidden while it has
    /// no messages, which is right for a session picker but wrong here: a
    /// `goose run` writes its session row before it has processed a turn, so a
    /// client asking which session it can attach to is told "none" and opens a
    /// parallel one, and the pane's human then types into a conversation that
    /// client never watches. And once it has messages it is merely one row among
    /// the leftovers of previous runs, where the most recently updated one can
    /// be a conversation nobody is driving.
    ///
    /// So the live session is listed even when empty, and flagged either way.
    /// Injected on the first page only, so paging cannot show it twice; a
    /// keyword search is left alone, since a client searching is not attaching.
    async fn mark_live_session(
        &self,
        session_infos: &mut Vec<SessionInfo>,
        may_inject: bool,
        cwd: Option<&std::path::Path>,
        session_types: &[SessionType],
    ) {
        let Some(live_run) = &self.live_run else {
            return;
        };
        let live = live_run.session_id.as_str();

        if let Some(listed) = session_infos
            .iter_mut()
            .find(|info| &*info.session_id.0 == live)
        {
            flag_live(listed);
            return;
        }
        if !may_inject {
            return;
        }

        let Ok(session) = self.services.session_manager.get_session(live, false).await else {
            return;
        };
        if !session_types.contains(&session.session_type)
            || cwd.is_some_and(|cwd| cwd != session.working_dir)
        {
            return;
        }

        let mut info = build_session_info(session);
        flag_live(&mut info);
        session_infos.insert(0, info);
    }
}

fn flag_live(info: &mut SessionInfo) {
    let mut meta = info.meta.take().unwrap_or_default();
    meta.insert("live".to_string(), serde_json::Value::Bool(true));
    info.meta = Some(meta);
}
