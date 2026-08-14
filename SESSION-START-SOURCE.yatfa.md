# Telling a SessionStart hook what triggered it

This argues for one small addition to goose's hook layer: a `source` field on the `SessionStart`
payload, saying whether the event came from a process start, a `/clear`, or a `/compact` — and
emitting the event on the latter two, which today do not reach hooks at all.

Written down because we needed it, built it here first, and think it belongs in goose rather than
in a fork. Same spirit as [ACP-ATTACH.yatfa.md](ACP-ATTACH.yatfa.md): the goose-side shape only,
nothing about our runner or how we deploy.

## The gap

`SessionStart` fires once, on the session's first agent turn. `/clear` replaces the conversation
and zeroes the token counters; `/compact` condenses it. Neither touches the hook layer, and neither
restarts the process — so a hook that set something up at session start never learns that the
context it set up for is gone.

The payload has no field describing the trigger either. A hook receives `event`, `session_id`, and
whichever of the tool/file/shell fields apply. Nothing distinguishes one `SessionStart` from
another, because today there is only one kind.

Claude Code delivers this field already: `SessionStart` with a `source` of `startup`, `resume`,
`clear` or `compact`, re-fired on clear. Anyone running both engines against one set of hook
scripts has to write the goose path as a special case.

## Why "just re-fire the hooks" is the wrong fix

The obvious fix — emit `SessionStart` again on `/clear` — is worse than the gap, and this is the
part worth reading even if you don't want the feature.

Session-start hooks are where you put setup that is safe *because the process is starting*. A
repository-syncing hook is the sharp example: at process start the working tree is disposable, so
`fetch` → `checkout -f` → `reset --hard` → `clean -fd` is exactly right, and anything more
conservative leaves the checkout stale forever. We run precisely that hook, and its unconditional
form was a deliberate fix for a conservative version that skipped on a dirty tree and therefore
skipped every session.

Mid-session that reasoning inverts. The process did not restart; the agent may hold uncommitted
work that exists in no pushed branch. Re-firing an unlabelled `SessionStart` on `/clear` would run
that hook against a live working tree and destroy the work silently — the user typed `/clear` to
drop their *context*, not their *code*.

A hook cannot defend itself here, because the two events are byte-identical. So the fix is not to
re-fire the event; it is to **say what happened and let the hook decide**. Cheap engine side,
and it moves a policy decision to the only layer that has the information to make it.

## The change

Three pieces, all small:

1. **`SessionStartSource`** — `startup` / `clear` / `compact`, serialized as `source` on the
   `SessionStart` payload. Names match Claude Code's, so one hook script reads one key under both
   engines. A payload with no source omits the key entirely, so every other event keeps its exact
   current shape and a consumer that ignores the field sees no change.

2. **`/clear` and `/compact` emit `SessionStart`** — from the agent's command handlers, so a
   programmatically-delivered command counts, and from the CLI's own clear handler.

3. **`SessionStart` fires exactly once per session, and the first one is always `startup`.**
   This is the load-bearing part, and it cuts in two directions.

   *No second `startup`.* `/clear` empties the conversation, so the *next* turn looks like a first
   agent turn again; unguarded, the reply path emits a second `startup` — the one value that tells
   a hook the working tree is disposable — for what is really a mid-session clear. The destructive
   path would then be reached *by the fix itself*. A test asserts the sequence is `[startup, clear]`
   and not `[startup, clear, startup]`.

   *No missing `startup`.* The converse is just as damaging and less obvious. A `/clear` can arrive
   *before* any turn has run — an embedder that opens a session and immediately clears it does
   exactly this, and it is the normal dispatch shape in our fleet. Labelling that first event
   `clear` means `startup` never fires at all for the life of the process, so a repo-syncing hook
   takes its conservative mid-session path at the one moment its unconditional path is correct,
   and the working tree keeps whatever the *previous* process left in it. So the first
   `SessionStart` of a session is reported as `startup` whatever triggered it: at that point
   nothing has run, and the tree is disposable by any useful definition. A second test asserts
   that a clear arriving before any turn still delivers `[startup]`.

`resume` is deliberately not emitted. Claude Code has it; nothing here consumes it, and a value
with no consumer is surface without benefit. Adding it later is additive.

Diff shape: one new enum and one optional field in `crates/goose/src/hooks/mod.rs`, one emit
helper plus a once-per-session set in `crates/goose/src/agents/agent.rs`, two call sites in
`crates/goose/src/agents/execute_commands.rs`, one in `crates/goose-cli/src/session/mod.rs`.

## What a maintainer would need to accept

Stating these plainly rather than hoping they don't come up:

- **Is this the right vocabulary?** We took Claude Code's because cross-engine hook scripts are the
  use case, and inventing a third dialect for the same three concepts helps nobody. If goose wants
  its own names, the mechanism is unaffected — it's a string in one `match`.
- **Should `source` be on `HookContext` at all, or should `SessionStart` get its own payload type?**
  We put it on the shared struct with `skip_serializing_if`, matching how `tool_name`,
  `working_dir` and the rest already work there. A per-event payload type would be a larger
  refactor of the hook layer and is a reasonable thing to want instead.
- **Is emitting on `/compact` right?** Compaction keeps a summarized conversation rather than
  emptying it, so it is a weaker reset than `/clear`. We emit both because they sit side by side in
  the CLI, Claude Code treats them as two values of one concept, and a hook that only cares about
  `clear` can simply test for it. Dropping `compact` would not change the mechanism.
- **Behaviour change surface.** Process start is untouched. The only new emissions are on `/clear`
  and `/compact`, which previously emitted nothing, so no existing hook can regress unless it was
  relying on those commands *not* reaching it.

## Status

Built on branch `pr/4-session-start-source`, over clean upstream `main`, as one self-contained
commit. Not yet filed upstream — that's a separate decision, and this document is written so it can
be the body of the issue when it is.
