# One agent, many frontends — measured 2026-10-23

Research record and plan for rendering a goose session in a browser from **structured events**
instead of the PTY screen, and for letting a terminal and a browser be two frontends over one
backend.

Everything in "Measured" was observed on a live engine today. Everything in "Plan" is a proposal.
Anything not observed is marked **unverified**.

## What was measured against

| | |
|---|---|
| engine | fork `main` at `v1.54.0-8-gaf669e05`, debug build, `--no-default-features --features "rustls-tls,acp-http"` |
| arrangement | `goose session` in tmux with `GOOSE_RUN_SERVE_ACP_PORT` — the fleet's arrangement, **not** `goose serve` |
| provider | hermetic stub Anthropic endpoint (`ANTHROPIC_HOST`), no credential |
| clients | two/three ACP WebSocket clients, all declaring `clientCapabilities._meta.goose.customNotifications = true` |
| isolation | own `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, own ports, own tmux session; the fleet was not touched |
| harness | `~/mf-probe/` (`run.sh`, `run2.sh`, `probe.py`, `probe2.py`, `stub.py`), derived from `yatfa-agent/spike/acp-shape` |

The fleet currently runs `GOOSE_FORK_TAG=yatfa-1.48.0-delta` (`yatfa-agent/bin/install-agent.sh:147`),
so the engine measured here is **six minor releases ahead of production**. There is no upstream
`GOOSE_VERSION` pin any more; the one quoted in older notes (1.45.0) is dead.

## Measured

### Cross-client visibility — the blocker

| turn driven by | driver client sees | second attached client sees | human in the pane sees |
|---|---|---|---|
| ACP client A | 14 frames (`agent_message_chunk`×7, `usage_update`×4, `session_info_update`×2, `message_usage`) | **0 frames** | everything (fork's TUI mirror) |
| the TUI (typed in the pane) | **0 frames** | **0 frames** | everything |

Both halves of upstream #10130 are therefore open, and the second half is worse than the first:
a turn typed in the pane is invisible to **every** socket client, including the one that is
otherwise driving the session. In code: notifications are emitted through `cx.send_notification`,
the handle of the connection that is serving the request (`crates/goose/src/acp/server.rs`), and
`Agent::reply()` returns a single-consumer `BoxStream` (`agents/agent.rs:2224`) — there is no
per-session fan-out anywhere.

The fork already has the one extra sink that exists: `AcpServer` carries a single `event_tap`
sender (`goose-cli/src/session/builder.rs:966`) which the ACP handler writes every turn to, and
the TUI drains it (`session/acp_mirror.rs`). It is server-wide, not per-connection — so it is the
natural place to generalise.

### Late join and replay — works today

`session/load` **replays the stored conversation as ordinary `session/update` notifications**:
12 frames for a 3-message session (`user_message_chunk`, `agent_message_chunk`, `message_usage`,
`usage_update`, `available_commands_update`). A client attaching mid-session gets history in order
without any new mechanism.

What is missing for recovery after a dropped connection: **no cursor**. `params` carries only
`sessionId` and `update`; ordering is arrival order. The only identity is
`update._meta.goose.messageId` plus `created` (epoch **seconds** — too coarse to order chunks).
A reconnecting browser can therefore replay everything, but cannot say "resume after X" without a
new field.

### Granularity — better than expected

Observed update kinds: `user_message_chunk`, `agent_message_chunk`, `tool_call`,
`tool_call_update`, `session_info_update`, `available_commands_update`, plus
`usage_update` / `message_usage` / `status_message` on `_goose/unstable/session/update`
(gated on the `customNotifications` capability). `agent_thought_chunk` exists in code
(`SessionUpdate::AgentThoughtChunk`) but the stub never emitted thinking — **unverified** live.

**Shell output streams.** A 5-second `for i in 1..5; echo; sleep 1` produced one `tool_call`
followed by six `tool_call_update` frames arriving ~1/s, each carrying a `live_output` chunk with
its own `sequence` counter:

```json
{"method":"session/update","params":{"sessionId":"…","update":{
  "_meta":{"toolNotification":{"type":"live_output",
    "params":{"chunks":[{"output":"line-1\n","stream":"stdout"}],"sequence":1,"truncated":false}}},
  "sessionUpdate":"tool_call_update","status":"in_progress","toolCallId":"toolu_stub_1"}}}
```

The `tool_call` frame itself carries `rawInput`, a human `title`, and
`_meta.goose.toolCall.{extensionName,toolName}` — enough to render a tool card without parsing
anything.

### Approvals — delivered to the driver, and the pane is a dead end

With `GOOSE_MODE=approve`, the tool call produced a `session/request_permission` **request**
addressed to the driving client A only; the passive client B received nothing. Meanwhile the pane
printed a bare

```
action_required(tool_confirmation): shell
```

— the mirror renders the event but offers no prompt, so the human cannot answer. This is a real
gap today, independent of the browser work: any fleet agent in approve mode on a dispatcher-driven
turn blocks on a decision only the bridge can make.

### Volume — the frame cap is irrelevant

52 frames: median 282 B, p90 367 B, **max 1994 B**. The bridge's cap is 8 MiB
(`acp/client.go:55`) — four orders of magnitude of headroom. Worst case is bounded by
`live_output` chunking, which the engine already truncates (`truncated` flag).

### sessions.db — incremental per message, not per turn

Message count went 3 → 4 one second into a turn (the user message, persisted immediately) → 5
after the turn (the assistant message). So the store is written **per message**, not at turn end,
but it has no chunk-level granularity: tailing it gives near-real-time *messages*, never live
token or tool output. It is a transcript source, not a streaming source.

### One more thing worth knowing

`session/list` is **empty until the session has content** — a freshly started `goose session` is
not listable, so an attach that races startup finds nothing and must retry. The bridge's ready
gate hides this today.

## Coverage table — where each TUI element comes from

| TUI element | available as an event? | notes |
|---|---|---|
| assistant text | yes, streamed | `agent_message_chunk` |
| thinking | yes in code | `agent_thought_chunk` — **unverified** live |
| tool call + args | yes | `tool_call` with `rawInput`, `title`, extension/tool names |
| live tool output | yes, streamed, sequenced | `tool_call_update` + `_meta.toolNotification.live_output` |
| tool result / status | yes | `tool_call_update.status` |
| context % / token count | yes | `usage_update`, `message_usage` (custom-notification capability) |
| available slash commands | yes | `available_commands_update` |
| active run id | yes | `session_info_update._meta.goose.activeRunId` |
| conversation history on attach | yes | replayed by `session/load` |
| **turns typed in the pane** | **no** | #10130 — invisible to all socket clients |
| **approval prompt** | request to the driver only | pane shows a dead-end line |
| slash-command *output* | not measured | **unverified**; rendered by the CLI, likely PTY-only |
| status line, spinners, progress bars | no | pure TUI rendering |
| queued messages, `/toggle`, interrupt echo | no | TUI input state |
| the input line being edited | no | rustyline-owned, see below |

### Serve-path parity — MEASURED 2026-10-24

Run with `~/mf-probe/run3.sh` and `run4.sh` (hermetic: own `GOOSE_PATH_ROOT`, XDG dirs, ports,
stub provider; `goose serve --dangerously-unauthenticated`).

| item | result |
|---|---|
| `session/new` costs model turns? | **no — 0 provider calls.** The boot recipe exists only to satisfy `goose run`'s clap "exactly one input" rule; on serve it has no job left, and the recipe-write + `goose recipe validate` + `--text` fallback machinery in `goose_recipe.go` all disappear. |
| slash commands over ACP | **work.** `/clear` sent as `session/prompt` executed with **0 provider calls** — they run inside `reply_impl` (`agents/agent.rs:2367`), so every client gets them, not just the CLI. |
| `SessionStart` hook | **fires**, with the fork's `source`. Sequence over 11 steps: `startup` (once) … `clear` (on `/clear`). A second connection, `session/load`, prompts from either connection, and a system-prompt set added **no** extra hooks. The invariant the repo-sync hook depends on holds. |
| when `startup` fires | **on the first prompt, not on `session/new`.** On the run path it fires at boot. Anything that assumes "hook ran before the first dispatch" must be re-checked. |
| `GOOSE_SYSTEM_PROMPT_FILE_PATH` | **not honored on serve.** It is read in the CLI session builder only (`goose-cli/src/session/builder.rs:622-633`). The fleet's entire role prompt travels this way, so this is the one hard gap. |
| replacement for it | **`_goose/unstable/session/system-prompt/set`** (`mode: "set"`) works and *replaces* the base prompt — the system block went 3758 → 126 chars carrying only our text. Same semantics as the fork's "never fall back", per session, re-settable without a restart. Better than the env file; the work moves to the bridge. |
| approval answered by an ACP client | **full round trip works.** `session/request_permission` → client answers `{"outcome":{"outcome":"selected","optionId":"allow_once"}}` → the tool ran and streamed 6 `tool_call_update` frames. This is the hardest thing the attach client must do, and it is already proven. |
| `session/list` right after `session/new` | **empty** — the session is not listable until it has content, on both paths. An attaching client must retry rather than conclude "no session". |
| cross-connection updates | still per-connection: A does not see B's turn. Unchanged — which is why the fan-out belongs in the bridge. |

One unexplained observation: an earlier run (`run3.sh`, which also exercised approve mode and a
system-prompt set) logged `startup, clear, startup`. Eleven targeted steps in `run4.sh` could not
reproduce the third hook. Not a blocker, but it is the one thing to watch when the sequence is
replayed against a real fleet config.

### The terminal attach client is nearly free

`crates/goose-cli/src/commands/roam_client.rs` is **283 lines** of upstream-maintained ACP client
that already does `initialize` → `session/list` → `session/load`, reads prompts from stdin,
renders `session/update` to the terminal, **and answers `session/request_permission`
interactively**. It speaks `ByteStreams`; pointing it at a WebSocket is the only transport work.
So the cost the spike's verdict (iv) quoted for Shape B — "the bridge must supply an ACP TUI
client, ~250 lines" — has already been paid upstream.

### On the fleet — MEASURED 2026-10-24 on `yatfa-planner-2`

Against the engine the fleet actually runs (`goose 1.48.0`, fork tag
`yatfa-1.48.0-delta`), attaching a second ACP client to the run-side port:

- `session/list` showed the live session (`sessionType=user`, 88 messages, `providerId=yatfa`).
- `session/load` replayed the whole conversation: **119 frames in 5.6 s**, including 29
  `tool_call` / 29 `tool_call_update` pairs.
- **Largest frame: 88 474 bytes (86 KiB)** — 40× the hermetic stand's worst case, still ~95× under
  the bridge's 8 MiB cap.
- A turn typed into the pane (`… reply with the single word ACK`) completed in 15 s and the
  attached client received **zero frames** for it. #10130 reproduces on the fleet engine, not only
  on a local build.

Then with `goose attach` (1.54 client against the 1.48 server — the protocol matched):

- it attached, replayed the prior turn, drove a new one, and rendered the reply;
- the tmux pane mirrored the same turn (`dispatcher ▸ …`).

Two frontends over one agent, on production hardware. Probe files were removed from the container
afterwards; nothing in the fleet was modified.

### Terminal attach client — BUILT 2026-10-24

`crates/goose-cli/src/commands/attach.rs` on branch `attach-client`. A WebSocket ACP client that
picks the session the way the bridge does (skip `sessionType=acp`, newest wins), loads it, renders
the update stream, sends typed lines as prompts, answers `session/request_permission` fail-closed,
and sends `session/cancel` on Ctrl+C. Live shell output renders line by line as it arrives.

Not yet exercised: the permission answer and Ctrl+C **through the client** (both proven at the
protocol level in `run3.sh`, neither driven through this code path). Rendering is flat — no
markdown, themes or spinners.

## Built 2026-10-24 — the complete path, across three repos

The decision taken: an interface that shows *almost* everything is not worth
having, so pane-typed turns are published to the socket rather than left
invisible. That makes the browser complete **without** removing the pane, and it
leaves "remove the pane" as a separate decision rather than a prerequisite.

### goose — pane turns reach attached clients (branch `attach-client`)

`crates/goose/src/acp/pane_turn.rs` is the mirror of `turn_event.rs` in the
other direction. `goose run` publishes its own reply stream on a broadcast; each
live ACP connection subscribes and forwards what it sees to its client, for
sessions that connection has loaded.

Deliberately notification-only: it registers no permission, mints no run id and
touches no session state. The pane owns its turn — it is already prompting its
human and already persisting the conversation — so a forwarder doing any of that
would be a second owner racing the first. Measured consequence, and the right
one: a pane-driven tool in `approve` mode pauses on the pane's own prompt while
the client watches the call sit pending.

The translation reuses the server's own converters
(`tool_calls::conversion`, `tool_notifications`) rather than a second copy, so a
forwarded turn is shaped exactly like a socket-driven one. Visibility of those
two modules was widened from `pub(super)` to `pub(crate)`; nothing else in the
ACP server changed.

Measured with `~/mf-probe/run.sh`, the acceptance criterion written above:

| turn | before | after |
|---|---|---|
| typed in the pane, passive client | **0 frames** | `user_message_chunk` + 7 `agent_message_chunk` |
| typed in the pane, with a tool | — | `tool_call` + **6** `tool_call_update` (live output, one per second) |
| driven by ACP client A, passive client B | 0 frames | still 0 — the socket→socket half of #10130, out of scope |

The remaining zero is deliberate: in the fleet the bridge holds the only ACP
connection, so socket→socket visibility buys nothing. An attach client that
drives its own turns is the case that would need it.

### agent-bridge — the event leg (branch `event-leg`, repo `yatfa-agent`)

`cmd/agent-bridge/event_leg.go`: the ACP read loop's `session/update` stream is
kept, numbered under the same per-process seq and boot generation the raw leg
uses, held in a 1 MiB ring, and published as `agent_event` frames on the
AgentChannel. The ACP setup was extracted so the leg connects at boot instead of
waiting for the first dispatch — `session/load` then seeds the ring with history.

Inert unless `YATFA_AGENT_EVENTS` is set. That is the property the rest of the
fleet relies on: the outbound cable write lives on a 500 ms budget with a
reconnect watchdog, and a real frame reached 86 KiB, so an un-opted agent pays
one atomic load per notification and nothing else. Five Go tests cover the gate,
retention when a send fails, and retention before the control socket exists.

### yatfa — the structured page (branch `session-events-page`, repo `yatfa`)

`/terminals/:agent_id/events`, a sibling of the terminal workspace, which is
untouched. `AgentChannel` relays `agent_event` the way it relays raw chunks;
frames are split for the broker's ~8 KB NOTIFY ceiling (a history frame measured
86 KiB) and the client applies an event only once every part has arrived. A
replay answer is relayed one frame per event, bounded at 500, and closed by an
envelope naming the ring's span.

Writing reuses `POST /terminals/:agent_id/input`, whose payload a goose agent
already receives as a conversational turn over ACP — so the page adds **no write
surface and no new authorization question**. Interrupt remains the existing
signal verb.

10 channel specs, 5 request specs, 11 jest specs (part reassembly, gap
re-request, generation replacement, rendering, composer). The whole existing
channel suite (316 + 286 examples) still passes.

### What is NOT done, and why

- **The engine is still `goose run`.** Switching it to `goose serve` and deleting
  the PTY leg needs a goose release into the sandbox image — a `git push` and a
  `GOOSE_FORK_TAG` bump — which is yours to authorize. Parity for that switch is
  measured above; the one gap (`GOOSE_SYSTEM_PROMPT_FILE_PATH` is CLI-only) has a
  better replacement already working, `_goose/unstable/session/system-prompt/set`,
  and that work belongs to the bridge.
- **Nothing is pushed.** Three branches exist locally, in three repos.
- **The end-to-end chain was verified hop by hop, not in one running fleet**:
  goose→client and goose→browser live (local, and on `yatfa-planner-2` for the
  read path), bridge→server by its own tests and the channel specs that feed the
  exact frames the bridge emits. A single live run through Rails needs the
  engine flip above.
- The browser page's rendering is first-draft; `goose attach`'s rendering is
  still flat (no markdown, themes or spinners).

## Prod trial 2026-10-24/25 — `yatfa-planner-2`, and the session split it exposed

Both patched binaries were installed on one idle prod planner through the
per-agent `env:` dev override (`YATFA_GOOSE_URL`, `YATFA_AGENT_BRIDGE_URL`,
`YATFA_AGENT_EVENTS=1`), which needs no release, no pin bump and no image
rebuild. No other agent was touched.

First run — **the forward path works, and nobody was listening.** A turn typed
in the pane reached a separately attached ACP client (`goose attach`) live. The
bridge saw none of it: it was attached to `20261024_2`, a session it had created
itself, while the pane drove `20261024_1`.

The cause is a one-second race with a permanent consequence. `goose run` writes its
session row, then starts the ACP server; the bridge asks `session/list` with
`only_sessions_with_messages: true` — correct for a session picker, wrong for
"what can I attach to", because a zero-turn boot recipe has no messages yet. The
list came back empty, the bridge fell back to `session/new`, and from then on the
dispatcher and the human were in two different conversations in one process. The
mirror case is worse: with leftovers present, newest-wins can pin a dead one.

Fixed on both sides, and the fix is what the engine knows that the client cannot
guess:

- goose (`acp/server/list_sessions.rs`) lists the session the run registered its
  agent under even while it is empty, and marks it `_meta.live: true` either way.
  Injected on the first unfiltered page only; `live_session_id` is `None` for
  plain `goose serve`, so the list is byte-for-byte upstream's.
- agent-bridge (`acp_dispatch.go`) prefers a `live` session over both its sticky
  pin and the newest-wins scan, and logs when it moves off a stale pin. An engine
  that does not send the flag resolves exactly as before.

Second run, observed in the pane and in `/tmp/agent-bridge-events.log`:

```
[BRIDGE] acp session/list raw result: … "sessionId":"20261024_3" … "messageCount":0 … "live":true …
[BRIDGE] acp session/load OK id=20261024_3
event leg: subscribed to session 20261024_3 (ring 0..0)
```

Then, in that one session: a pane-typed turn appeared in an attached client
(`two clients one pane ok.`), and a client-sent turn appeared in the pane as
`dispatcher ▸ … / mirror ok.` One agent, one conversation, two frontends, on
prod. Bridge status stayed `✅`, zero event-leg write failures, display leg
`failed=0`.

**Inferred, not observed:** that each forwarded notification landed in the event
leg's ring. The leg logs subscription and failures only, and the consumer that
would show the frames — `/terminals/:id/events` — is not deployed. Delivery to
the bridge's connection is observed (same session, same build, a second client
on it saw the frames); the ring append is covered by its Go tests.

## The option that changes the plan: no TUI at all

Everything above assumes the pane stays. If the browser becomes the *only* human surface, the
problem inverts — the work gets smaller, and most of this fork stops being needed.

**Why the delta exists at all is the pane.** Of the four pieces in `ACP-ATTACH.yatfa.md`, piece 3
(serve-on-run) exists only because `goose serve` has no human surface, and piece 4 (the TUI
mirror) exists only to render socket turns in that pane. `GOOSE_CLI_EXIT_ON_INTERRUPT` is a TUI
keybinding concern; `READLINE-RESIZE.yatfa.md` is a rustyline concern. Drop the pane and all of it
goes away, along with the bridge code that exists purely to serve a PTY (`resync_screen.go`,
`raw_replay.go`, `terminal_geometry.go`, `pty_resize`, the mixed-width machinery).

**The measured blocker also goes away.** The reason a passive client sees nothing is that turns
originate in a connection it is not. If no turn ever originates in a pane, the only remaining case
is socket-to-socket — and if the **bridge holds the single ACP connection** and fans out to N
browsers itself, goose needs no fan-out at all. Stage 1 moves out of goose core and into the
bridge: no upstream dependency, no rebase cost, no core ownership.

```
goose serve  ──one ACP connection──  agent-bridge  ──WS──  browser 1..N
 (near-stock)                        event log, cursor,
                                     fan-out, approval owner
```

The bridge becomes the backend of record for the UI. That is also where the resume cursor belongs
— it can be assigned on arrival, so goose needs no new field either.

### Serve-path parity — what was checked in code today

| needed | status |
|---|---|
| `SessionStart` hook with `source` (`startup`/`clear`/`compact`) | **fires in core**, not in the CLI — `agents/execute_commands.rs:217,254` and `agents/agent.rs:558`. The repo-sync hook keeps working on the serve path. |
| slash commands | present over ACP — `available_commands_update` plus execution of `UnstructuredCommandInput` (`acp/response_builder.rs:449,636`). **Not measured.** |
| recipes | the ACP server has a `recipe` module and `recipeParameterRequests` capability (`acp/server.rs:124,449`). **Not measured** — the fleet boots from a recipe, so this is the one that must be proven first. |
| approvals | `session/request_permission` goes to the connection holding the session; with one bridge connection, the bridge owns it unambiguously. Measured. |
| interrupt | `session/cancel` — no PTY, and no risk of a stray Ctrl+C killing a supervised run. |

### What is actually lost

- **Terminal access when the web is down.** The replacement is not the TUI: a small ACP client
  (the bridge already has one in Go) or a one-shot `goose run -t` is enough. Upstream shipped and
  then deprecated exactly such a thing in `ui/text/` (#10799) — the precedent is that an ACP TUI
  is easy and the full interactive CLI is the expensive part.
- **Operator habit.** `docker exec … tmux attach` is how the fleet is operated today. This is a
  product decision about how operators work, not only an engineering one.
- **Unknowns in `goose serve`.** We have never run the fleet on it: extension load timing,
  scheduler-off behaviour, and `sessionType=acp` on every session (the bridge currently uses that
  field to *skip* its own sessions — `acp/client.go` `SessionInfoMeta`) and the transcript uploader
  reads the same store.

### Recommended sequence if this direction is taken

0. **Serve-path parity probe** (half a day, same harness): boot `goose serve` from the fleet's
   recipe, run a slash command, verify the `SessionStart` hook fires with `source=startup` and
   again with `clear`/`compact`, answer an approval over ACP, confirm `sessionType` and store
   layout. This is the only thing standing between the plan and a decision — everything else is
   already known.
1. Bridge-side event log + cursor + fan-out to browsers (no goose change).
2. Browser conversation view; keep the xterm view alive against the existing PTY path until it is
   ahead.
3. Retire the PTY leg: the pane, the mirror, serve-on-run, and the PTY code in the bridge. The
   fork shrinks to the handful of core fixes that have nothing to do with terminals.

Stages 1-5 below remain the plan **only if the pane stays**. They are kept because the decision
above has not been made.

## Verdict

**Feasible with caveats.** The event stream is rich enough to render a conversation view —
streaming text, tool cards with live output, usage, commands, and full history on attach — and the
volume is trivial. Two things must be built before a browser can be trusted as a frontend:

1. **Per-session fan-out of turn events.** Without it the browser sees only what the bridge itself
   dispatched, and a human typing in the pane is invisible. This is the whole decision.
2. **A resume cursor.** Replay works; resuming after a dropped socket does not.

And one thing must be decided rather than built: **who owns approvals** when a pane and a browser
are both attached.

What is *not* feasible: treating the event stream as a drop-in replacement for the screen. The
status line, spinners and the input line are renderings, not protocol. A browser frontend is a
*different* view of the same session, not the same pixels.

## Minimal schema a browser needs

```
session.loaded      { sessionId, cursor }
message.chunk       { messageId, role: user|assistant|thought, text, cursor }
message.usage       { messageId, inputTokens, outputTokens, contextUsed, contextLimit }
tool.call           { toolCallId, messageId, extension, tool, title, rawInput, cursor }
tool.output         { toolCallId, sequence, stream: stdout|stderr, text, truncated }
tool.status         { toolCallId, status: pending|in_progress|completed|failed, result? }
turn.state          { runId, state: idle|running|awaiting_approval|cancelled }
approval.request    { requestId, toolCallId, options[], owner }
commands.available  { commands[] }
```

`cursor` is the only field goose does not emit today. Everything else is a rename of a field
already on the wire.

## Plan

### Stage 0 — cheap fixes that stand alone (days)
- `renderScrollback` writes `\r\n` per stored line (`resync_screen.go:451`), turning soft wraps
  into hard breaks: this is why **Rebuild** does not fix narrow-width history. Fix independently
  of everything below.
- The readline width corruption has its own evidence file (`READLINE-RESIZE.yatfa.md`) and a
  deterministic cause; it is not an argument for the browser work.

Doing these first means the terminal stops being the thing we are escaping from.

### Stage 1 — per-session event fan-out (the decision)
Replace "one owner of the reply stream + one hardcoded tap" with a per-session broadcast that
every renderer subscribes to: the TUI, each ACP connection, the bridge. Shape it as upstream
#10130 would, because `main` rebases onto release tags and a bespoke construction in this area
makes every rebase expensive.

Acceptance: re-run `~/mf-probe/run.sh` and see a non-zero `passive_B_kinds` for both the ACP turn
and the **pane-typed** turn. That is the one number this whole plan turns on.

### Stage 2 — cursor and resume
Add a monotonic per-session sequence to every update and accept it on `session/load`
(`resumeFrom`). Browser recovery becomes: reconnect → load with the last cursor → done. Without
this, every reconnect either re-renders the world or loses frames — which is the "missing final
frame" symptom from the brief, now with a mechanism instead of a guess.

### Stage 3 — approval ownership
Decide and implement: one owner answers, everyone else watches the decision arrive. Today the
pane's `action_required(...)` line is a dead end; at minimum it should say who is being asked.

### Stage 4 — browser conversation view
Render the schema above. Keep the xterm view alongside it until the new one is ahead; they are
different views, so there is no cutover to get wrong.

### Stage 5 — input over ACP instead of keystrokes
| action | path | needs a PTY? |
|---|---|---|
| send a message | `session/prompt` / `steer` | no — and it echoes to every frontend as `user_message_chunk`, which PTY echo cannot do |
| interrupt | `session/cancel` | no — safer than Ctrl+C, which can kill a supervised run (`GOOSE_CLI_EXIT_ON_INTERRUPT` exists for exactly that) |
| slash commands | commands from `available_commands_update` | no |
| live line editing | rustyline | yes, see below |

## The shared input line

Holding the state is easy; sharing it with the *terminal* is not.

A composing buffer is `{text, cursor, author, version}` with last-writer-wins — trivial for
browser↔browser (the bridge owns it) and trivial to broadcast. The hard part is that in the pane
the buffer lives inside rustyline (v18, `custom-bindings`; goose already binds sequences in
`goose-cli/src/session/input.rs:185-223`), and rustyline offers no way to inject a buffer
mid-`readline` or to observe it per keystroke. Sharing the *edit* with the TUI therefore means
either driving rustyline from custom bindings on every key, or replacing the line editor — a much
larger change than Stages 1-2, and in the opposite direction from "the browser is not a terminal".

Recommendation: do not share the edited line with the TUI. Broadcast only "who is composing" and
committed prompts. Browser↔browser sharing of a live buffer is a separate, self-contained feature
if it is ever wanted.

## Risks

- **The fork stops being thin.** After Stage 1 the fork owns the agent's event fan-out and a
  renderer. The "keep the delta small" rule no longer applies, and rebases onto release tags get
  more expensive. Worth saying out loud before starting, not after.
- **Two frontends, one agent, divergent permission state.** Stage 3 is not optional once a browser
  can drive turns.
- **Schema drift into yatfa tooling.** Sessions are snapshotted by the transcript uploader; a
  storage-side change ripples outward. Stages 1-2 touch the wire, not the store — keep it that way.
- **Fleet is six releases behind the measured engine.** Any plan step verified here must be
  re-verified on the tag the fleet actually installs, or the pin must move first.

## Rough size, if the verdict is accepted

| stage | goose | bridge | web |
|---|---|---|---|
| 0 | — | small | — |
| 1 | medium (core) | small | — |
| 2 | small | small | small |
| 3 | medium | medium | medium |
| 4 | — | medium | large |
| 5 | small | medium | medium |

## Reproducing

```bash
cd ~/goose && source bin/activate-hermit
cargo build -p goose-cli --bin goose --no-default-features --features "rustls-tls,acp-http"
~/mf-probe/run.sh              # P0-P6: visibility, replay, sizes, sessions.db
MODE=auto    ~/mf-probe/run2.sh  # tool-call granularity, live output streaming
MODE=approve ~/mf-probe/run2.sh  # where session/request_permission is delivered
```

Results land in `~/mf-probe/out*/results*.json`. No credential, no fleet access, nothing written
outside the harness directory.
