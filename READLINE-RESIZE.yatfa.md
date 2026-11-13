# Readline corruption after a terminal resize — what we know

Findings from the live breakage on `yatfa-planner-2` (2026-09-07). The user's input line
duplicated, "ate" characters and walked up the screen while typing; Ctrl+L made it worse.
This file records what was proven, what was ruled out, and how to recover, so the next
occurrence starts from evidence instead of theories.

## Symptom

While typing at the goose prompt, characters land mid-word, whole buffer renders stack up
on screen (one copy per keystroke or Ctrl+L), the line drifts upward, and Enter appears to
do nothing. Comes and goes; hits any agent, including ones that never receive dispatcher
traffic — which is why the dispatcher-turn mirror was suspected first and is now cleared.

## The chain

macmini → Terminal.app → `docker exec -it -u yatfa <container> tmux attach -t agent` →
inner tmux session `agent`, pane on `pts/0` → bash → agent-bridge → `pty.Start(goose)` on
a **separate pty** (`pts/1`) → `goose run --interactive`.

agent-bridge is a manual PTY relay: pane stdin → goose pty, goose pty → pane stdout
(`streamPTYOutput`, byte-for-byte plus a Rails WS stream), and SIGWINCH on the pane →
`pty.Setsize` on the goose pty. The human sees goose's TUI only through this relay.

## What was measured on the live broken agent

- Every width in the chain agreed at the moment of measurement: Terminal.app 75x308,
  tmux window/pane 308x74, goose pty 74x308. The width-mismatch theory is dead.
- The pane showed the user's multi-line buffer rendered ~11 times, stacked. Each redraw
  appended instead of replacing: rustyline's redraw was computed for the wrong geometry.
- The decisive detail: the buffer line hard-wrapped mid-word at column **~101**
  (`…значение роадмапа. ес / ли человек…`) while the real width was 308. The running
  readline believed `cols ≈ 100`.
- The goose pty itself was already 308 — the bridge forwards resizes correctly. The link
  that missed the resize is **rustyline inside the running readline**: it never re-read
  the size no matter how many resizes landed on the pty.
- Ctrl+L only redraws with the stale width; it does not re-read it. That is why every
  Ctrl+L added another copy.
- The user's typed reply had never reached the session DB — the buffer was never
  submitted; the garbage (` nj nj221`, a stray blank line) sat inside the readline buffer.

## The fix that works live

Toggle the window width by one column — SIGWINCH, then one more:

```bash
tmux resize-window -t agent -x $((width-1)) && sleep 1 && tmux resize-window -t agent -x $width
```

On planner-2 this made rustyline re-read the size and redraw correctly in place, verified
stable over three captures / 24 s. The buffer content (including any garbage typed while
corrupted) is preserved. Ctrl+C once drops the line for a clean retype — the fleet runs
`GOOSE_CLI_EXIT_ON_INTERRUPT=false`, so one press does not kill the run.

Ctrl+L does not fix it. Re-attaching usually fixes it, because a fresh attach resizes the
pane — which is why it "sometimes just goes away".

## Ruled out, with the reason

- Dispatcher-turn mirror: breaks with zero dispatcher traffic; also the relay means the
  mirror writes to `pts/1`, not to the human's pane.
- Width mismatch in the chain: all four widths measured equal.
- Bridge relay dropping bytes: output streams byte-for-byte; sizes forwarded (pty was 308
  while readline rendered at 101 — relay did its job, readline ignored it).
- Fork paste/Enter handlers: the whole paste-capture path is `#[cfg(windows)]`; on Linux
  `console_pending_events() -> 0`, so Enter is always plain `AcceptLine`.
- Orphaned spinners: no spinner glyph visible during the breakage.
- Stale cached width across readlines: rustyline creates a fresh renderer per `readline()`
  (`lib.rs` `create_writer`), so the stale width lived inside ONE long-lived readline.

## Root cause — found 2026-09-08 (rustyline 18.0.1)

Two independent width sources disagree:

- Agent output re-reads the winsize per render (`console::Term::stdout().size_checked()`)
  — always current.
- The input line reads the width once per `readline()` (`PosixRenderer::new` →
  `get_win_size`, `tty/unix.rs`) and afterwards updates it only in the `Signal::Resize`
  arm of `State::next_cmd` (`edit.rs:103-117`).

That handler is fed through a self-pipe, and the pipe is consumed **only on an EINTR on
the reader thread itself** (`TtyIn::read`, `tty/unix.rs:147`; `poll()`, `tty/unix.rs:736`
— on the Linux poll path the pipe is not even in the poll fd set). The kill chain fails
one step earlier, though: **`kill(pid, SIGWINCH)` is process-directed, and the kernel
picks the delivery thread — preferring the group leader.** In goose the readline does not
run on the leader: the input loop runs on the named thread `goose-cli-main`, while the
leader (and the tokio workers) sit in futex waits. So the signal — and the handler's pipe
byte — land on a thread that is not reading the tty; the reader thread is never EINTR'd,
never checks the pipe, never re-reads the width.

Evidence from the live planner-3 specimen (2026-09-08): handler installed
(`SigCgt` bit 27 set), fd 0/1/2 all `pts/1`, ioctl reports the current size — yet a burst
of 12 `kill -WINCH` produced zero re-reads, while an injected keystroke rendered fine
(input works because the tty wakes the reader with data, no signals involved).
Delivery to the readline thread is a lottery on thread states — which is exactly why the
breakage is intermittent and why a resize toggle sometimes fixes it (one signal in the
burst happens to reach the reader thread).

The fleet makes mid-readline resizes routine: several live attaches of different widths
per session (planner-3 had `s000` and `s008` from different terminals), tmux reconciles
the window per its `window-size` policy, and every reconcile mid-typing triggers the
lottery.

## Fix direction

- Deterministic, bridge-side (yatfa-agent, no forks): after forwarding a resize, enumerate
  `/proc/<engine_pid>/task/` and `tgkill(pid, tid, SIGWINCH)` **every thread**. The reader
  thread is guaranteed a recipient → EINTR → pipe byte consumed → `update_size()` →
  refresh. Extra deliveries are benign: the handler only writes a token byte (extra bytes
  drain as harmless no-op `Resize` events), and tokio futex waits tolerate EINTR as a
  spurious wakeup. ~15 lines of Go.
- Proper upstream fix in rustyline: include the sig pipe in the poll fd set on the Linux
  poll path (the macOS `select()` path already does), so the reader wakes on pipe data
  regardless of which thread received the signal. Not required once the bridge shim ships.
- Nothing to change in goose itself.

## Recovery playbook when it happens again

1. Deterministic unstick: deliver SIGWINCH directly to the engine's threads, e.g. from the
   mac: `docker exec -u yatfa <container> sh -c 'for t in /proc/$(pgrep -f "goose
   run" | head -1)/task/*; do kill -WINCH $(basename $t); done'` — per-thread `kill` is
   thread-directed and always reaches the reader (plain `kill -WINCH <pid>` is the
   lottery that caused the bug).
2. Or the width toggle (`resize-window -x N-1` then `-x N`) — probabilistic, worked by
   luck so far. **Caveat:** `-x` puts the tmux window into manual size mode and it stops
   following the client — restore with `tmux set-window-option -t agent window-size
   latest`, otherwise the next fullscreen/attach looks "completely broken" (planner-3,
   2026-09-08).
3. `Ctrl+C` once drops a corrupted line for retype (fleet runs
   `GOOSE_CLI_EXIT_ON_INTERRUPT=false`).

## Live proof — 2026-09-08, planner-3

A stuck (stale-width) readline was healed without restarting anything:

```bash
TID=$(docker exec -u yatfa <ctr> sh -c 'for t in /proc/<pid>/task/*; do
  [ "$(cat $t/comm)" = goose-cli-main ] && basename $t; done')
docker exec -u yatfa <ctr> kill -WINCH $TID
```

One thread-directed SIGWINCH → the reader thread EINTR'd → pipe byte consumed →
`update_size()` → re-render at the real width. The user then stress-tested: long typing
wraps exactly at the pane edge and Up/Down history navigation no longer eats rows.
Process-directed `kill -WINCH <pid>` did nothing in 14 attempts before that — the
thread-directed delivery is the difference. This is also the exact behavior the proposed
agent-bridge fix (tgkill all engine threads on every forwarded resize) automates.

## The Up-arrow ratchet — confirmed live 2026-09-08

With a stale-width readline, history navigation eats output line by line, in one
direction only. Measured on planner-3 with single presses:

- **Up = exactly one eaten row per press.** The refresh erases `layout.end.row` rows
  above the cursor using the stale cols≈100 bookkeeping; the real layout is wider, so
  the erase overshoots one row up every cycle and the prompt climbs into the agent's
  answer. Two presses → two rows gone, both predicted and confirmed by capture diffs.
- **Down = harmless, any number of presses.** The buffer grows downward and the same
  arithmetic error lands below/into blank space.

What is lost is only the visible pane grid. The answer text lives in `sessions.db`, and
tmux scrollback above the visible area is untouched.

## Diagnostic one-liners that produced the evidence

```bash
ssh macmini
D=/usr/local/bin/docker
$D exec -u yatfa yatfa-planner-2 tmux display -p -t agent '#{window_width}x#{window_height}'
$D exec -u yatfa yatfa-planner-2 tmux list-panes -t agent -F '#{pane_width}x#{pane_height} pid=#{pane_pid} tty=#{pane_tty}'
$D exec -u yatfa yatfa-planner-2 sh -c 'stty -F /proc/<goose_pid>/fd/0 size'   # goose pty winsize
$D exec -u yatfa yatfa-planner-2 tmux capture-pane -p -t agent | grep -nv '^$'  # what the human actually sees
# did a submitted message reach the model?
echo <sql> | base64 -d | $D exec -i -u yatfa yatfa-planner-2 sh -c 'sqlite3 ~/.local/share/goose/sessions/sessions.db'
```

`capture-pane` is the ground truth for "what the user sees"; the DB is the ground truth
for "what the model received". Comparing the two settles every dispute about whether a
loss happened on input or output.
