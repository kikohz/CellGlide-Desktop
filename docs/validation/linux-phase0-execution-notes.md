# 第 0 阶段执行记录：判定与遗留问题

逐任务子代理执行（2026-10-14/15）期间做的判定，以及审查中记下、留到 Linux 首版处理的小问题。原始台账在已删除的临时工作区里，这里是完整副本。

## 判定

- CI triggers on every push (branches too) — feature-branch pushes must run CI for T2/T13 — cost if wrong: extra CI runs.
- nix moved to [target.'cfg(unix)'.dependencies] in cg-service — nix does not build on Windows and windows-check builds the workspace — cost if wrong: none functional.
- cg-gui gated to cfg(unix) with a stub main elsewhere — cg_ipc::client is unix-only — cost if wrong: Windows GUI starts from scratch later anyway.
- Run resolved probe and privileged tests in a dedicated local Lima VM `cellglide-test` (Ubuntu 24.04, aarch64; repo visible at the same path via Lima's home mount) while CI billing is blocked; CI jobs stay in the workflow — cost if wrong: aarch64 VM instead of x86 GitHub runner (same Ubuntu/resolved version), CI evidence missing until billing is fixed.
- Accept Co-Authored-By naming the model that actually wrote a commit (e.g. Haiku 5.5) instead of rewriting history — attribution stays truthful — cost if wrong: cosmetic trailer inconsistency
- Task 11 backoff follows the brief's stated 2,4,8,16,30 s (brief code capped at 16) — prose states intent, code was the bug — cost if wrong: slower retries after 4 failures
- hev-sys build.rs re-copies the submodule on every run and watches all of hev/ (brief copied once, watched hev/src) — avoids linking stale objects after a bump — cost if wrong: full hev rebuild whenever the build script reruns
- Task 16 Step 8 (desktop eyeball in Parallels) replaced by an Xvfb run + screenshot in the Lima VM; tray host behaviour on a real GNOME/KDE desktop moves to Task 17's real-device checklist row 9 — no desktop VM is available — cost if wrong: tray/Wayland issues surface only at real-device testing
- Task 16 splits service_problem / command_error so command failures stay visible (brief cleared all problems on every snapshot) — UX defect, spec §7 needs the switch to be usable — cost if wrong: one extra field

## 留到首版的小问题

- Task 1: minor (deferred): Cargo.lock committed (not in brief); path deps to future crates
- Task 2: minor (deferred): network-snapshot.sh:66 sed regex unescaped (IPv6 brackets/dots) + dead second substitution — may cause false "changed" in Task 13 with IPv6 servers
- Task 2: minor (deferred): validation doc should say CI re-run pending
- Tasks 3-5: minor (deferred): HTTP/1.1-only status prefix uncommented; empty computer name omits header; thin tests (Listen/ReadPairRecord encode, wrong service); connect_domain_request accepts empty host; dict.clone in encode_message
- Task 6: minor (deferred): no boundary tests for read_line (exact MAX_LINE, EOF w/o newline); client has no connect timeout; subscribe ends silently on parse error
- Task 8: minor (deferred): fake watch wrong-session answers 403 (real phone 409); watch write/open outside handshake timeout; status/metrics tests rely on 100ms sleep; fake accept loops not aborted
- Task 9: minor (deferred): no handshake timeout on local SOCKS greeting (idle client holds a slot); UDP control watcher ends on any byte not just EOF; send_to error ends association; no tests for MAX_CLIENTS / in-flight stop / requested port; silent handle errors
- Task 10: minor (deferred): accept/recv error loops busy-spin (sleep before continue); no tests for DoH stale-retry / TCP release; hev_config lacks port-range check; failed exchange on healthy idle sender duplicates query; regression test uses fixed 100ms
- Task 11: minor (deferred): Forwarder::start failure leaks watch task (use abort-on-drop guard); actor unresponsive during establish (up to ~18 s; Disconnect queued); interval Burst ticks; stale Metrics after teardown repopulate snapshot (add active.is_some()); Connect{same serial} needlessly reconnects; chosen-but-absent phone shows Connecting; tunnel.stop() failure only logged; NoPhone shown between relistens
- Task 12: minor (deferred): make only gets CC (no AR/CFLAGS for cross builds); .git file copied into OUT_DIR; clones need --recursive (README hint); linux-test.sh always --workspace so -p doesn't narrow
- Task 13: minor (deferred): Running has no Drop and hev stop-timeout closes fd while hev may still use it (fix before daemon hardening); possible sticky SYNC_STOP from late quit; ioctl passes &ifreq not &mut; no IFNAMSIZ assert; second_tunnel test only asserts is_err; IPv6 try_for_each stops half-way; privileged test orphans hold_tunnel child on mid-test panic
- Task 14: minor (deferred): accept loop spins on EMFILE without log; getgrouplist blocking in accept loop (slow NSS stalls all); closed subscribers linger until next snapshot change; one denial message for all causes; unconditional socket unlink; SIGHUP kills without teardown
- Task 15: minor (deferred): "Error: ✗" anyhow prefix; raw English OS error in service-down text; doctor has no ✓ group line when member (spec §6); NoUsbService text says auto-start even if usbmuxd missing; connect may return on stale snapshot and exits 0 on Retrying/NoPhone; doctor blames group for any service Error; passing doctor lines read as requirements
- Task 16: minor (deferred): tray try_send drops on full channel (Quit could be lost); tray not recreated if host dies; empty readings text takes space; raw OS error text in UI; stale command_error persists after recovery; no "手机名" in window (Snapshot has serials only); tray/close/reopen/Quit untested (real desktop)
- Task 17: minor (deferred): is-active misses auto-restart window; $USER unset under set -u / sudo adds root; uninstall rm without cellglide0 check; after-snapshot races service setup; template rows look prefilled
- Final: minor (deferred): tray_up never resets if tray host disappears; window closed before TrayReady exits app; hev abort path untested; metrics-guard test non-discriminating

## 仍未完成

- x86 实机 + iPhone 测试（计划 Task 17 Step 6–7），结果填入 docs/validation/linux-phase0.md。
- GitHub Actions 因账单问题未运行；处理后需重跑 CI（linux、windows-check、resolved-probe、privileged）。
