# Linux 1.0 M4 界面：执行记录

按子代理逐任务执行时控制者所做的判定，以及审查中留下、未在本里程碑处理的小问题。原始台账在执行完成后删除，这里是它的存档。格式：判定 — 理由 — 判错的代价。

## 判定

- Ruling P1 (T7 vs pages): T7 defines each page's `State` and `Msg` with exactly the variants `route` and T7's tests need (history::Msg::Loaded, diagnostics::Msg::Prepared, settings::Msg::{Saved,Checks}, leak::Msg::Checks); page tasks add the rest — route must compile in T7 — cost: none.
- Ruling P2 (T7/T9/T10 tray): T7 owns all tray.rs edits needed for its new Message shape (both OpenWindow sites → OpenWindow(None); handshake not_in_group). activate→TogglePanel belongs to T9 only; T10 does icons/menu only — one owner per change — cost: none.
- Ruling P3 (T5/T11): `Run` (and what it carries) derives Clone in T5 — iced Message needs Clone — cost: none.
- Ruling P4 (T7/T9 time): `Message::Tick(Instant)` from iced::time::every; panel/toggle logic takes std Instant; wall-clock values use jiff::Timestamp::now() where needed. The tick runs whenever a window or panel is open — cost: none.
- Ruling P5 (T7 window events): map only Focused/Unfocused/CloseRequested/Closed into Message::Window; ignore RedrawRequested and the rest — avoids per-frame updates — cost: none.
- Ruling P6 (selectable text): iced 0.14 has no selectable text. Where the plan says 可选中, use read-only text plus a "复制" button (clipboard::write) — cost: user copies whole blocks, not partial selections.
- Ruling P7 (T7 key scan): the scan collects only `tr("…")`/key-shaped literals of known namespaces and ignores file names; exclude strings ending in .json/.desktop/.svg — cost: none.
- Ruling P8 (deps): `tempfile` as a dev-dependency of cg-gui is allowed (dev-only; already in workspace lock via other crates) — cost: none.
- Ruling P9 (T13 midnight): on day change the page reloads today (history request for the new today), never requests a day outside the server's 7-day window; Loaded(Err) clears loading and keeps a retry button; test reflects that — cost: none.
- Ruling P10 (T13/T6 duration): history rows show event_title only (it already carries the duration for End events); no separate right-hand duration — cost: none.
- Ruling P11 (macOS manual checks): drop the macOS speed-test-over-Wi‑Fi and macOS panel "目测" steps; those are covered on Linux (Lima/test machine) in T17 — tray and bind are Linux-only — cost: none.
- Ruling P12 (T17 row f): "dimmed card" applies while the frame is stale (<6 s, PhoneFeed stale=true); after GONE_AFTER the card shows usb-only data or nothing — row f wording follows that; T6 `dim` = card.stale — cost: wording only.
- Ruling P13 (T17 dpkg): install the exact file by version from scripts/pkg-version.sh, never a glob — cost: none.
- Ruling P14 (T2 tests): the controller test must observe the judge reset (e.g. an open issue is ended/suspended on Sleep, or the monitor's warmup restarts), using a timing with a non-zero warmup in that test — a no-op must fail — cost: none.
- Ruling P15 (T4): Snapshot::default() (no phone at all) still has no Phone line; "not connected" only when a usb phone or label exists but no frame — keeps the existing test — cost: none.
- Ruling P16 (T10 SVG): symbolic icons use `fill="currentColor"` only (no fixed #bebebe on the same element); the phone frame drawn with stroke="currentColor"; the orange dot is a separate element with a fixed fill and class "warning" — T17 row d is the real check on GNOME/KDE — cost: dot may be recoloured on some themes.
- Ruling P17 (T11 history carrier): carrier/radio for speed history come from PhoneCard data (carrier name, radio), not from CardText.title; None when unknown — cost: none.
- Ruling P18 (duplication): (a) T6 snapshot_items replaces snapshot_line's internals — one implementation, CLI keeps its line by joining items; (b) T5 moves progress_line/numbers/direction into cg_tools::speed::text so CLI and GUI share them; (c) config_home_from lives once in cg-tools (or cg-ipc) and both prefs and speed history use it; (d) one shared `checks` widget/state for doctor checks used by T12 and T15 (T12 creates, T15 reuses); (e) T15 doesn't re-test config_home_from — cost: slightly larger T5/T6.
- Ruling P19 (T12 ECS): T12 adds an ECS flag to ResolverGroup in cg-tools (set when any resolver in the group passes a subnet) and the page shows the leak.subnet sentence — cost: none.
- Ruling P20 (T13 clock): T6 also makes `describe::clock` pub — cost: none.
- Ruling P21 (T5/T11 progress): on_progress callback passes the leg index as well: `FnMut(usize, &str, &Progress)` — cost: CLI adapts.
- Ruling P22 (linux-test): any task touching cfg(linux) GUI/tray code runs scripts/linux-test.sh (T7, T9, T10 at least) — cost: time.
- Ruling P23 (T14 save_dir): `save_dir(xdg_download: Option<PathBuf>, home: PathBuf, exists: impl Fn(&Path)->bool)`; XDG download dir if set and exists, else ~/Downloads if exists, else ~ — cost: none.
- Ruling P24 (T11–T16 tests): T7 provides `Ctx::for_test(snapshot: Option<Snapshot>)` plus fixtures `connected_snapshot()` / `disconnected_snapshot()` in a `#[cfg(test)] fixtures` module; "phone connected" = ConnState::Connected with a PhoneCard — cost: none.
- Ruling P25 (Review Focus 1): add tests: history Loaded(Err) clears loading (T13); settings Saved(Err) ends saving with error (T15); pages recover after resubscribe/ServiceDown→Up (T7) — cost: none.
- Ruling P26 (T5 Important): one process-wide test lock for the language, cg_i18n::lock_for_tests(); cg-tools/cg-cli tests use it — split per-module mutexes raced after the move — cost: a tiny always-compiled helper in cg-i18n.
- Ruling P27 (T6 note): T7's key-scan test covers cg-gui sources only; cg-ipc's format!-built gui.state.* keys are covered by cg-ipc's every_state_has_title_and_detail — cost: none.
- Ruling P28 (T7 Important, plan-mandated `image` feature): use iced `image-without-codecs`; the About icon is shown via Handle::from_rgba from RGBA prepared at build time (T16) — full `image` pulled ~80 crates (rav1e, exr, tiff) for one PNG — cost: T16 adds a tiny build step.
- Ruling P29 (T9): English card title overview.connection = "Connection" (Mac key's translation "Connect" reads as a verb) — cost: none.
- Ruling P30 (supersedes P16's stroke clause): tray SVGs use filled outline shapes only (ring via fill-rule evenodd, arcs as filled bands), no stroke; orange dot stays a separate class="warning" fill — GTK's symbolic loader forces fill !important and ignores stroke/color, so stroked shapes would render solid or dark — cost: redraw four small SVGs.
- Ruling P31 (supersedes P9's "pull to today"): at midnight keep the selected day if still within [today-6, today] and re-request it to refresh `days`; only a day that left the window (or following-today) moves to today — matches Mac HistoryPage canGo/shift — cost: none.
- Ruling P32: expand/collapse markers use "+" / "−" instead of U+2304 — may be missing from iced's default font on Linux — cost: plainer look.
- Ruling P33 (T16): lwIP sources are all compiled (build.mk rwildcard), so the license list carries the full text of every distinct license comment block under lwip/src (deduped, with file lists), not just holder names — BSD-style notices require the text in binary distributions — cost: a longer list.
- Ruling P34 (T17): ship a systemd-sleep hook (root) that runs `cellglide-service resume-check` detached via systemd-run 3 s after resume; it restarts usbmuxd only if sysfs has an Apple usbmux interface and usbmuxd lists nothing for 10 s — service gets no new privilege — cost if wrong: an extra usbmuxd restart after resume (harmless when the phone was lost anyway).
- Ruling P35 (final): single instance via session-bus name com.cellglide.Gui with an Activate method (existing instance opens its main window); a second launch without --background activates and exits, with --background just exits; if the session bus is unavailable, run normally — cost: one more D-Bus surface in the GUI.
- Ruling P36 (final): fix wave includes minors 1–7 (resume spurious plug-in, resolver test retry, diagnostics '?' rows, install-dev hook, hook sandbox props, any-device note, health-line consistency); minor 8 (sync file I/O on UI thread) deferred to M5 — cost: possible brief UI stall on slow home dirs.

## 留下的小问题

- Task 1: minor (deferred): redundant tunnel.is_some filter on socks_port; no assert that socks_port is None while Retrying/Connecting
- Task 2: minor (deferred): wake path (Sleep{false}) not distinguished by the test; no reconnect if system bus unavailable at start/restarts; real logind check in T17 row l
- Task 3: minor (deferred): no test with events in the repeated fall-back hour; flat tick fallback duplicated in describe.rs; Monday weekday not asserted
- Task 4: minor (deferred): no test for SIMs without carrier suffix / carrier None; no 24 h boundary test; ECS sentence tested only in zh
- Task 6: minor (deferred): event_tone Issue w/o cause → Orange (Mac gray); no Escalate-severe tone test; no main-cause tie test
- Task 7: minor (deferred): P25 test weak until page tasks; prefs rename without dir fsync; prefs save failure only on stderr
- Task 7: minor (deferred): GUI not looked at on screen yet (T17); timeout test not run red
- Task 9: minor (deferred): Focused before PanelOpened is dropped (record focus for pending_id); a to-be-closed window may draw one frame of main content
- Task 10: minor (deferred): no icon-cache refresh in rpm post (Fedora filetrigger/dpkg trigger handle it); Connect/Disconnect always enabled even with service down
- Task 11: minor (deferred): progress try_send may drop the last Done tick (rare); history date not localized; secondary header colour; mid-transfer unplug may show ClosedEarly/Io not NotConnected (check T17)
- Task 11: minor (deferred): Enter as default for Start (accessibility; needs page key listener) — T17 list; misplaced doc comment on TEST_HISTORY; Finished not tied to a run id (unreachable today)
- Task 12: minor (deferred): no Copied feedback on leak page; sharing captured at Start; allow(dead_code) in checks.rs; view test only builds
- Task 13: minor (deferred): legend swatches round (Mac rounded squares); batched entered+show_window and auto-open paths covered by guard only
- Task 14: minor (deferred): Saving has no progress text; send_needs_preview on its own line; text_editor looks enabled while busy; -2 suffix sorts before original
- Task 14: minor (deferred): nested brace import `use cg_diag::{upload::{upload}}` not caught by text scan (rustfmt never emits it); partial-file removal untested
- Task 15: minor (deferred): login switch looks pressable while busy; shared copy timer; thread panic shows service_timeout text; symlink target mode becomes 0644; devices briefly empty on relock may count as plug-in (check T17)
- Task 16: minor (deferred): notice extraction is a heuristic (first Copyright comment per file); "iced"/"ksni" substring license test is weak; release checklist must re-run update-licenses.sh on any Cargo.lock change
- Final review minor 8 (deferred to M5, P36): prefs and speed history are written synchronously on the UI thread (app.rs prefs::save, speed.rs HistoryStore) — may stall briefly on slow home directories.
- Single instance: a GUI started before upgrading to this build does not hold com.cellglide.Gui, so the first launch after upgrade adds a second tray icon until the old process exits (log out or quit from the tray). Mention in M5 release notes.
- Post-resume plug-in suppression: unplugging and replugging the same phone within 60 s of wake does not open the window (documented trade-off).
