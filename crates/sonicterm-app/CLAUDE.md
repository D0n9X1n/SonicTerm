# sonicterm-app

## Purpose
Cross-platform application glue around `sonicterm-app-core`. This crate
owns the winit `ApplicationHandler`, window lifecycle, keymap dispatch,
PTY thread wiring, redraw scheduling, explicit config reload, overlays, tab
drag/tear-out, and the platform shell abstractions.

## Key files
- `src/app/mod.rs` - `App`, `UserEvent`, the winit handler, and the window-registration
  chokepoint. Window, pane, session, input, and effect orchestration live in
  `window_state.rs`, `window_registry.rs`, `pane_state.rs`, `session.rs`,
  `input_dispatch.rs`, and `effects.rs`.
- `src/app/window_event.rs` - `WindowEvent` dispatch, main redraw, shared pointer/wheel helpers.
- `src/app/window_keyboard.rs` - source-window keyboard, IME, focus, search, READONLY routing.
- `src/app/field_input.rs`, `field_pointer.rs` - source-field clipboard/selection commands and press-owned query dragging; field IME uses presented renderer caret geometry.
- `src/app/window_pointer.rs` - main-window cursor, wheel and left-button handlers.
- `src/app/splitter_input.rs` - main and child pane-divider hit-tests, hover and drag.
- `src/app/keymap_dispatch.rs` - action execution and READONLY whitelist.
- `src/app/event_loop.rs` - window creation and window-ready hooks.
- `src/app/spawn_pane.rs` - PTY thread pump and redraw coalescing.
- `src/app/reaper_driver.rs` - one App-owned native PTY teardown driver and retained transport custody.
- `src/app/path_target.rs` - contextual target resolution, openability probes, and direct-open workers.
  `path_target/unix.rs` is the command runner the macOS and Linux openers share;
  `path_target/macos.rs`, `path_target/linux.rs` and `path_target/windows.rs` hold each
  platform's probes and direct-open.
- `src/app/tab_transfer.rs` - pure GPU-free `TabContainer` transfer/reorder helper for tab movement tests, and the `App::transfer_tab` wrapper.
- `src/app/tab_state.rs` - `TabState`, main-tab navigation, and production `App` tab-state attach/detach helpers for main and child windows.
- `src/app/tab_widths.rs` - the hold rule for measured tab widths: the window pointer it reads, the frame outcome that keeps or restores them, and the redraw that applies held widths after a release.
- `src/app/tab_gesture.rs` - tab-bar press, motion and release routing (`WindowState::route_tab_*`) and the `App::apply_tab_*` steps both pointer handlers share.
- `src/app/tear_out.rs` - native tear-out drag and child-window lifecycle; drop targets and OS
  drag handoff live in `tear_out/drag_target.rs` and `tear_out/os_handoff.rs`.
- `src/app/shared_gpu.rs` - the committed GPU context every later renderer shares, and the GPU device-state waker.
- `src/app/gpu_recovery.rs`, `gpu_recovery_worker.rs` - event-loop recovery ownership and one persistent nonblocking request worker.
- `src/app/child_window.rs` - child-window event routing, redraw gating, and resizing.
- `src/app/child_window_redraw.rs` - child frame collection, render, IME anchor and tab-bar snapshot.
- `src/app/child_window_pointer.rs` - child pointer chrome, hover, selection, left-button and wheel routing.
- `src/app/child_tabs.rs` - child tab and pane operations and child PTY/VT wiring.
- `src/app/config_apply.rs` - explicit reload of `~/.sonicterm/sonicterm.toml`.
- `src/app/redraw.rs` - owner-local causes, pre-lock output snapshots, outcome settlement,
  structural/device suppression, and typed due-owner service.
- `src/app/visible_frame.rs` - validated visible-only frame handles, non-blocking guards,
  media snapshots, and shared `PaneRender` assembly for both window roles.
- `src/app/viewport_anchor.rs` - scrolled-back viewport anchor rebased across history eviction.
- `src/app/selection_gesture.rs` - local selection gestures bound to their press pane and anchor, click counting, and
  the pointer-gesture types.
- `src/shell.rs` - shared shell runner with thin macOS, Windows, and Linux builders.

## Local gate
```bash
cargo build -p sonicterm-app
```

## Guardrails
- All pane destruction uses `retire_pane`; transfers preserve the PTY and its
  `ReapSlot`. Reserved native waits run only through the one `ReaperDriver`.
  Admission refusal retries once, then logs/counts synchronous fallback.
- Retired native custody owns one process-root `PtyTransport` and `ReaperWork`
  item until actual settlement or process-exit sink retention, independent of
  the former window/pane lifetime. `QueueFull` may last until process exit.
- `finish_session` retires every window's panes, including hidden main, before
  shutdown control. The shared shell calls it on both run outcomes. Preserve the
  original result; only actual teardown settlement permits a clean-session marker.
- Render paths use `try_lock`, not blocking `lock`; avoid AB-BA deadlocks
  with PTY/parser work on the main thread.
- Frame collection validates unique live leaves and active/zoom agreement before
  capturing visible handles. Owned sources outlive borrowed parser guards; all visible
  parsers are acquired before visible image snapshots. This is not an atomic grid/media
  generation. Hidden parser/image stores are neither locked nor cloned for a frame.
  Only genuine contention enters the retry floor; structural invalidity skips the
  whole assembly with a bounded window warning and a typed non-retry result.
- Keep PTY redraw coalescing burst-aware; never redraw per byte. OSC 52 writes
  must stay bounded and reach the native clipboard only on the event-loop thread;
  clipboard reads/queries remain unsupported.
- Main and child keyboard, IME, focus and modifier events share source-WindowId
  ownership. Search input has priority over READONLY; quick-select retains its
  hint keys. In READONLY, only the explicit safe action whitelist may execute.
- Select the native drop owner before every main, warm, tear-out or new window
  is created. Failed registration must precede PTY startup or pane transfer.
- Every window after the first uses `App::shared_gpu_context`, including warm-pool
  and tear-out windows. Recovery owns the committed context; a discarded partial
  rebind must never become the context for another window.
- Recovery prepares and commits all live/warm renderers in one callback, retires
  failed candidates before dispatch resumes, and never joins its request worker.
- Do not add unconditional heartbeat redraws at the tail of event handling.
- Pane output generations publish after complete batches; the collector Acquire-loads
  identities before locking. Frame completion settles only captured owner generations.
  `last_render` stays the sole pacing clock and `request_redraw(&self)` stays native-only.
  Structural parking excludes every frame deadline; Output maintains commands but cannot
  unpark. Device-stop reporting runs before this suppression. Native evidence is separate
  from the fake-clock and source-contract tests.
- A scrolled-back viewport is anchored to history identity. Writers repin through
  the pane's anchor setter with a baseline read under the lock that chose the row;
  readers resolve through the anchor, and both render collectors reconcile every
  held pane before reading `viewport_top_abs`, which stays a compatibility projection.
- A local selection drag belongs to its press pane: motion maps through that pane's
  rendered column edges and clamps to its addressable cells, a contended press or one
  on a cell the held grid lacks starts no gesture, and ownership is checked before any
  layout lookup, so a removed pane or tab, any tab switch, a screen change, an evicted
  anchor, or any real resize of the press pane's grid (`Grid::size_generation`) cancels
  the drag instead of retargeting it.
- Per-pane budgets do not impose a process quota; process and window owners
  are tracking-only. Inline media has a 256 MiB process target plus a possible
  4 MiB newest-image residual per live pane. Decode-time trimming and the
  idle-pane walk enforce distinct parts of that policy; tests must identify
  the mechanism they exercise, not just assert an aggregate bound.
- Reclamation that destroys something the user can see logs on
  `memory::reclaimed`, which is admitted at every level including the
  default. Diagnostics belong on `memory`, which is off unless someone is
  investigating.
- Tab widths are measured only in the two redraw paths, right before
  `render_with_outcome`; pointer, drag, tear-out and snapshot paths read the stored
  widths. A title, badge or privilege change is held while any window has a pressed
  or dragged tab, or while that window's own pointer rests on the bar; the dispatcher
  records the pointer (`record_window_pointer`) on every move and leave before any
  overlay or handler. A redraw whose frame does not present restores the widths and
  limits still on screen (`settle_tab_widths`). Startup (`session.rs`) and live reload
  (`config_apply.rs`) set `tab_min_width` and `tab_max_width` as the process-wide
  limits; each measurement pass records them on the bar, and layout reads the bar's.
  Config apply hands both limits to their setters on every reload, which ignore invalid
  values. A test that reloads them runs inside `with_scoped_tab_width_limits`, which
  keeps them on its thread. Both pointer handlers route tab-bar presses, moves and releases
  through `WindowState::route_tab_press`, `route_tab_motion` and `route_tab_release`, and
  `App::apply_tab_*` carries the result out; the handlers supply only the bar layout and,
  for a tear-out, the event loop.
- Window-ready hooks fire once, immediately after winit creates the window.
- Every terminal window enforces the shared 30-column by 10-row native inner-size
  floor from live renderer geometry and refreshes it after metric/DPI changes.
- Local-target hover never performs filesystem I/O on the event-loop thread.
  Clickability requires a current epoch-keyed typed navigation-or-reveal result.
  All platforms navigate directories and select files in their containing folder;
  local OSC 8 destinations must use the same probes, never the URI opener.
  Unverified bare names neither preview nor trigger failure notifications or clipboard writes.
  Explicit filepath failures show the path and reason on modifier-click without copying;
  a second modifier-click on the same failed path while its error is visible copies and
  reports the actual clipboard result. Only current validated targets dispatch native actions.
  File type, executable mode, and content never prevent reveal-only selection. macOS package
  directories are selected rather than launched. Hover never copies; native failures return
  only to the originating window/pane. Native dispatch revalidates identity and kind,
  retaining locality and special-file protections. macOS and Linux follow symlinks.
  Windows resolves each drive letter once with `QueryDosDeviceW`, walks only an exact
  `\Device\HarddiskVolume<N>` whose root reports a local disk, opens each later part by one
  name below its held parent (`OBJ_DONT_REPARSE`, no delete sharing; `FILE_READ_DATA`, or
  `FILE_EXECUTE` only when reading is denied, never write or delete access), and holds every
  part until the check or the shell call ends. It follows a symlink or junction only between
  local fixed disks and never opens a remote volume; it refuses mapped-network, `subst`,
  optical, RAM-disk, dynamic-disk, shadow-copy, unmapped-letter, volume-GUID, UNC and device
  targets before opening anything they name,
  and other reparse points and paths needing more than 31 link hops. It hands the shell the
  walked link-free path; the shell then opens that path itself, the final part can still
  change in place, and a process in the user's own logon session is out of scope.
- Contextual terminal candidates, including names containing ordinary spaces,
  resolve only against the exact pane's trustworthy local OSC 7 CWD, after OSC 8,
  URI, and explicit-path precedence; never fall back to process CWD, another pane,
  or HOME. Candidate enumeration and background probes stay explicitly bounded.
- Path detection is one component for every operating system, because a Windows
  pane can show POSIX paths, for example from a WSL shell. Grammar code in
  `path_target.rs` branches on a `PathStyle` value, never on `cfg`; the native
  probes, reveal and open live in `path_target/{unix,macos,linux,windows}.rs`.
  Every place the app chooses a grammar uses `PathStyle::native()`, so a Windows
  build scans a WSL pane with the Windows grammar.
- Wrapped plain-text local targets join recorded automatic wraps and, on the alternate
  screen, inferred pane-edge continuations, at most 32 visible rows
  (`MAX_WRAPPED_PATH_ROWS` = `MAX_HOVERED_URL_SPANS`) and 16 KiB of logical-line text
  (`MAX_LOGICAL_LINE_BYTES`), so a 1024-character target fits at ordinary widths. The
  hard-wrap bracketed URL body stays at 4 KiB (`MAX_LOGICAL_PATH_BYTES`), as does the
  scanner's per-target cap.
  Authorization binds every row hash/wrap bit, ordered absolute spans, pointed cell,
  viewport, screen epoch, eviction generation, and pane CWD; hard lines on the primary
  screen, incomplete chains, unsafe cells, or any identity change fail closed.
- Path candidates longer than `clickable_path_max_chars` Unicode scalars (default 1024,
  clamped to 1..=1024) are dropped. `probe_candidates` orders the rest shortest first by
  scalar count of the displayed candidate, ties to the earlier start; a tier whose
  candidates are all missing is skipped, and in the first tier with any present candidate
  a blocked one refuses (`path-error-blocked`) while otherwise the earliest actionable one
  wins. No candidate depends on a longer literal being absent. Only the selected path is
  highlighted. Auto-detected text whose candidates all name no file leaves a modifier-click
  as an ordinary terminal click (debug log only); OSC 8 and file-URI destinations still
  report missing. The open worker repeats the selection over every candidate at or before
  the selected tier (`open_request_still_selected`) and opens only on the same path and
  decision; `key_preserves_selection` drops authorization when a new candidate appears at
  or before that tier. Reloading a changed cap revokes path results.
- On the alternate screen a multiplexer places each pane row with a cursor move. The
  plain-target scan reads only the pointed pane's columns of each row, so a pane border
  ends every name as the grid's edge does. A recorded wrap happens only at the grid's
  edge and joins only one pane's text: the pane reaching the right edge to the pane
  starting at the left edge, when either row is unsplit (`wrap_joins_one_pane`). Because
  a pane-edge wrap leaves no recorded bit, `inferred_continuation_below`/`_above` also
  join a segment whose last column holds text to the next visible row's segment with the
  same pane edges when it starts with text, unless a recorded wrap enters that row from
  another pane; chains stop at 32 rows and the view edges. These joins carry URIs as
  well as paths, because tmux separates rows with CR LF when it redraws; unrelated rows
  that exactly fill a pane edge can join into a longer URI, and the modifier-hover
  preview shows the full destination before activation. `spans_reach_cut_pane_edge` checks against the joined chain
  rows: when the pointed unspaced run reaches a stopped pane edge the scan is refused, and
  any other candidate that reaches one is filtered out individually. The primary screen
  joins only recorded wraps. A bracketed URL never joins two rows across a pane border that
  both rows draw. On the alternate screen, `hyperlink_hover_cells` continues one OSC 8
  link's fragment into the next row's fragment of the same link when, inside one pane, at
  most `LINK_CONTINUATION_MARGIN` (2) blank cells follow the upper fragment before the
  pane's right edge and only blank indentation of at most `LINK_CONTINUATION_INDENT` (8)
  cells precedes the lower one, covering multiplexer pane edges and hanging-indent wraps
  such as Claude Code's; repeated short links on consecutive rows stay separate. On the
  primary screen only a recorded soft wrap continues an OSC 8 underline. This changes
  only the underline, because activation opens the stored destination, not
  joined text. Relative and contextual targets get no OSC 7 CWD on the alternate screen
  (`relative_text_cwd`): a multiplexer relays only its active pane's directory, and its
  pane borders may be box drawing, ASCII or blank, or look like a program's own rule, so
  the screen cannot show which pane holds the text.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-vt`, `sonicterm-grid`,
  `sonicterm-io`, `sonicterm-cfg`, `sonicterm-render-model`,
  `sonicterm-ui`, `sonicterm-gpu`.
- Consumed by: `sonicterm-mac`, `sonicterm-windows`, `sonicterm-linux`.
