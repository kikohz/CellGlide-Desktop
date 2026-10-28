# M1 执行记录：判定与遗留问题

逐任务子代理执行 M1（2026-10-15）期间做的判定，以及审查记下、留到后续里程碑处理的小问题。原始台账在已删除的临时工作区里，这里是完整副本。

## 判定

- Shared client-error rendering lives in cg_ipc::describe (unreachable, version_mismatch, service_check) instead of per-binary copies — one implementation for CLI/tray/app — cost if wrong: none
- Turning auto_connect off keeps the running session wanted (manual = active||connecting); command replies after publish — brief's SetSettings/ack order were defects; spec §3.2 — cost if wrong: none
- Move the full health check off the actor (spawned future, result returned as an Event tagged with the generation) as an extra step in Task 7 — plan-mandated inline checks can stall Disconnect ~15 s exactly when cellular is down — establish stays inline (larger change, later) — cost if wrong: one more event type
- Bypass routes carry metric 32766 and are re-added on every default-route event (old removed only when the gateway changes) — brief's metric-0 routes hijacked users' same-prefix routes and same-gateway reconnects lost routes — cost if wrong: none; leftovers from unreleased metric-less builds aren't cleaned (none shipped)
- Residual parked for M2 Task 1: turning auto_connect off while idle/no phone sets manual = wants() = true, so a later plug-in still connects; fix: manual = was_wanted && (active || retry pending || last_error) — small UX inconsistency, no network risk — cost if wrong: one surprise auto-connect after turning auto off with no phone

## 遗留小问题

- Task 1: minor (deferred): test uses macro-private _rust_i18n_backend_translate; KEYS hand-maintained (yml keys not checked against it); tr() substitutes args one at a time (a value containing %{x} can be re-expanded); LC_ALL=C beats LANG=zh (plan-mandated)
- Task 2: minor (deferred): trust serials joined with ", " server-side (Chinese loses 、); no test that every Reason/check key exists in KEYS; v1 wire assertions dropped; group-problem text built inline twice; multi-line error in GUI banner; raw OS error inside %{error}
- Task 3: minor (deferred): non-NotFound read error overwrites settings with defaults; no dir fsync after rename; spec's 10 s disconnect timeout not implemented (client side); no test for Disconnect during establish; usbmuxd restart / unplug-before-detach keeps manual with auto off (accepted); CLI --phone "" accepted
- Task 4: minor (deferred): exit_failure_once_is_retried reads `before` before set_ok(false) (tiny vacuous-pass window); 200 ms subscription waits in metrics tests; HttpsProbe doesn't check reply[0]==5; stale reason on unplug during check; Tunnel variant reused for bad probe host
- Task 5: minor (carry into Task 7): sysfs speed "-1"/NaN → reported as speed=0 — guard mbits.is_finite() && > 0; real iPhone serial format to confirm in Task 8
- Task 6: minor (deferred): default gateway may pick another VPN's default (Mac picks en* only) — log chosen dev; add_all partial failure not retried until next event; realm silently dropped without CONFIG_IP_ROUTE_CLASSID (verify list_ours after add); 255.255.255.255 broadcast still into tunnel; start-to-monitor window; cleanup only deletes metric-32766 leftovers; nothing tests ControllerConfig.bypass propagation
- Task 7: minor (deferred): a panicking check task leaves *_running stuck (no more checks that session); stale_full_check_result_is_ignored doesn't discriminate the generation guard; closed_subscriber_task_ends relies on runtime alive-task count; full-check period now measured from start
- Final: minor (deferred): Closed → literal "closed" in %{error} (unlocalized); monitor spawns `ip route show` per link/address event (no debounce); optional 60 s resync skipped
- Task 8: minor (deferred): 'no default route outside cellglide0' warned 20× during boot (log once per state); snapshot script should exclude realm 198 routes

## 说明

- Task 2: deviations accepted: old-service Hello parse error → version mismatch; NotInGroup → group check (reviewer confirmed correct)
- Task 4: deviations accepted: full_check returns a Send future with copied inputs; exit_failure_once_is_retried polls (snapshot doesn't change on one failure)
- Task 4: note: worst-case release after cellular loss ≈ 60+8+5+8 ≈ 81 s (exitOK=false shortens it); spec-conformant
- Task 7: note: establish still inline on the actor (Disconnect during connect waits up to ~8 s)
- Final: re-review's race claim on auto_off test rejected — Controller::send awaits the ack sent after publish (Task 3); follow() holds the mutex for the whole read/apply (Task 6).
- Final: minor (deferred): Closed → literal "closed" in %{error} (unlocalized); monitor spawns `ip route show` per link/address event (no debounce); optional 60 s resync skipped
