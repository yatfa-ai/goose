# Attaching a program to a live goose session — the shape we're aiming at

This describes how we think an interactive goose session and a program driving it *should* fit
together, and which parts of that belong in goose rather than in this fork. It covers the
goose-side shape only — nothing here is about our runner, our control plane, or how we deploy.

Written down because the pieces are being discussed in several places upstream and the whole
picture doesn't live in any one of them.

## The shape

A session has one agent. That agent can have more than one door:

- a **pane** — a terminal a human can attach to and type into;
- a **socket** — ACP, for programs: an editor, a supervisor, a monitor, a transcript builder.

Whoever comes through either door is talking to the same agent, and everyone attached sees what
the agent does, regardless of who asked for it.

That's the whole idea. Most of what's missing to get there is already upstream work.

## The pieces

**1. Process-scoped agent state.** Today `AcpServer` builds a fresh `AgentManager` per ACP
connection (`transport/mod.rs:189` → `server.rs:2274` → `create_agent()`), so two clients — or one
client that reconnects — end up with two agents for one session id, both writing to the same
stored conversation. `GooseAcpAgent` is *correctly* per-connection: it holds the client's
handshake state and the handle back to that client. What's misplaced is the process state it also
owns — `agent_manager`, and with it `sessions`, `active_prompt_runs`, `closed_session_ids`.

This is a bug in goose, not a feature we need. It's the piece we most want upstream, and the one
worth doing first.

**2. Updates reaching clients that didn't start the turn.** Upstream #10130. Without it, a client
that attaches to observe a session sees nothing until it drives something itself. With it, the
"monitor / audit log / transcript" case needs nothing else — no steering, no CLI involvement.

**3. A pane process that also accepts a socket.** `goose serve` has no human surface, and #10799
removed the alternative, so `goose run` is the only maintained terminal. Behind
`GOOSE_RUN_SERVE_ACP_PORT` it starts an ACP server bound to loopback, sharing the session's agent,
scheduler off, existing secret-key auth. Off by default; unset means today's `goose run`
byte-for-byte.

This is the smallest thing that would let a human and a program share one session, and it is the
piece upstream is least likely to want — the CLI is deliberately minimal for scripting. It may
simply live here.

**4. Rendering another client's turn in the pane.** A turn arriving over the socket is invisible to
someone attached to the terminal. We duplicate the turn's events into the owning session and render
them through the same path a locally typed turn uses, dropping events rather than delaying the
client's reply. Fork-only, and not proposed upstream: it is a second, larger change to the same CLI
surface, and it buys the *live* view only — a transcript built from events is better off without
it. If the interactive CLI ever becomes an ACP client itself (discussion #7697), this stops
existing as a separate problem and becomes #10130.

## What you get with which subset

| Have | A program can | A human attached to the pane sees |
|---|---|---|
| 1 | reach the right agent, instead of a second one | their own turns |
| 1 + 2 | observe a session it isn't driving | their own turns |
| 1 + 3 | drive the same agent a human is watching, and read its turns as events rather than scraped output | their own turns |
| 1 + 2 + 3 + 4 | all of the above | everything, whoever asked for it |

Pieces 1 and 2 are goose's own; 3 is small and may stay here; 4 is ours.

## Why not just read the terminal

Because a pane is a rendering, not a protocol. Scraping it means parsing back what was formatted
for a human — message boundaries, tool calls and completion are all inferred — and writing to it
races the TUI's own input handling. The event stream gives the same information as structure. That
is the actual argument for the socket, and it holds even when nobody is watching the pane.

## Status

| Piece | Where |
|---|---|
| 1 — process-scoped agent state | upstream issue pending; branch `pr/1-share-agent` predates the design discussion and will be rewritten to this shape |
| 2 — cross-client updates | upstream #10130, not ours |
| 3 — attach on run | branch `pr/2-serve-on-run` |
| 4 — pane mirror | branch `pr/3-tui-mirror` |

Use case and background: aaif-goose/goose#11000.
