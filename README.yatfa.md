# yatfa fork of goose

Private fork of [aaif-goose/goose](https://github.com/aaif-goose/goose) at tag `v1.45.0`.

## Why this fork exists

A single change, on branch `yatfa/run-shares-acp` (also pushed to `main`):

**An interactive `goose run` exposes its agent over ACP, on a loopback port, while keeping its TUI.**

Set `GOOSE_RUN_SERVE_ACP_PORT=<port>` and `goose run` spawns an ACP server after the session is built. The agent the run is about to drive is the same `Arc<Agent>` the server returns for that session id — so an ACP client (`session/load` + `steer`) drives the live agent, not a freshly-built twin.

This is the load-bearing seam for yatfa: a dispatcher can inject into a busy agent (steer) without taking the human's TUI away. Upstream `goose` forces a choice between an interactive `goose run` (TUI, no ACP) and `goose serve` (ACP, no TUI). yatfa needs both at once.

## The change

- `AgentManager::insert_existing_agent(id, Arc<Agent>)` — register a pre-built agent under a session id; later `get_or_create_*` for that id returns the same `Arc`.
- `GooseAcpAgentOptions` / `AcpServerFactoryConfig` accept an optional pre-built `AgentManager`. When set, the ACP server reuses it (and its `SessionManager` / `PermissionManager`) instead of building a fresh one.
- `build_session` reads `GOOSE_RUN_SERVE_ACP_PORT`; when set, it constructs a manager, registers the run's agent, and spawns an `axum::serve` loop on `127.0.0.1:<port>`.
- `CliSession.agent` is `Arc<Agent>` (was owned `Agent`); the `Arc::try_unwrap` at the builder boundary is gone.

## Test coverage

- `execution::manager::shared_agent_tests` — `Arc::ptr_eq` between the registered owner and the manager's resolution.
- `session::builder::serve_acp_tests` — `maybe_serve_acp_on_run` opens the port and answers ACP `initialize`.

## Keeping this fork in sync

`upstream` remote points at `aaif-goose/goose`. To rebase onto a new release:

```
git fetch upstream --tags
git checkout yatfa/run-shares-acp
git rebase v<new-tag>
```

The patch touches three files and one new function; rebase conflicts should be minimal.
