# Linux 1.0 M3 数据与服务：执行记录

按子代理逐任务执行时控制者所做的判定，以及审查中留下、未在本里程碑处理的小问题。原始台账在执行完成后删除，这里是它的存档。格式：判定 — 理由 — 判错的代价。

## 判定

- Ruling P1 (T6): `event_json_matches_the_mac` expects the real value of Timestamp::from_second(1_791_000_000) = "2026-10-03T04:00:00Z" — plan arithmetic slip — cost: none.
- Ruling P2 (T1): removing Snapshot.metrics includes updating every user: cg-cli main.rs:73 metrics_line, describe.rs test, controller tests using s.metrics — workspace must build at T1 — cost: slightly larger T1.
- Ruling P3 (T8/T9): `Contact { email: Option<String>, note: Option<String> }` is a wire type defined in cg-ipc by T8; cg-diag uses cg_ipc::Contact; T9 consumes it — one definition — cost: none.
- Ruling P4 (T3/T5): `charging()` fixture exposed as `cg_core::testing::battery_fixtures::charging()` by T3 — T5 needs it — cost: none.
- Ruling P5 (T7): T7 adds key `history.date`; the four Linux-only event rows (service_started, bypass_unavailable, bypass_update_failed, dns_not_authorized) get zh/en written by the implementer — cost: copy review.
- Ruling P6 (T11/T12): leak test keeps `ToolError`, speed test keeps its own `SpeedError`; T11's "Task 12 继续加" note is void — cost: none.
- Ruling P7 (T12): add `serde` feature to workspace uuid and `tokio-util` to cg-tools deps — persistence needs them — cost: none.
- Ruling P8 (T12/T13, Focus 4): with --compare and physical_uplink() None, return an explicit NoInterface reason for the Wi‑Fi/Ethernet leg (shown to the user), phone leg runs; never fall back to unbound socket — Review Focus 4 binds — cost: none.
- Ruling P9 (T14): key-completeness tests live in the crate owning the enums (cg-ipc for ipc enums, cg-tools for tools enums), calling cg_i18n — cg-i18n cannot depend on them — cost: none.
- Ruling P10 (T7…T14 Lima): VM `cellglide-test` no longer exists. The first task needing it recreates it: `limactl create --name cellglide-test template://ubuntu-24.04` (home mounted read-only, rust via rustup), CARGO_TARGET_DIR=$HOME/cg-target. install-dev.sh must honour CARGO_TARGET_DIR (adapt script if it hardcodes target/) — read-only mount — cost: one-time VM setup time.
- Ruling P11 (T4): controller step; copy phone_probe to /usr/local/bin before `sudo -u cellglide` — /root not traversable — cost: none.
- Ruling P12 (T10): CellProxy work in a separate git worktree on a new branch `diag-linux` from origin/main (another session is working on branch luci-app in that checkout) — never touch the other checkout — cost: user merges diag-linux later; deploy happens from the worktree.
- Ruling P13 (T6/T7): event kinds stay camelCase (bypassUnavailable, bypassUpdateFailed, dnsNotAuthorized, serviceStarted) overriding 1.0 spec §3.6 snake_case — same field style as Mac bundles the server/tools already read — cost: rename later if wrong.
- Ruling P14 (T9 vs Global): Global sentence reads as "no capabilities beyond CAP_NET_ADMIN; read-only systemd-journal group allowed (1.0 spec §3.5)" — spec already lists it — cost: none.
- Ruling P15 (T9): diagnostics `environment` values stay English — support data like the Mac bundle, not UI copy — cost: preview shows some English.
- Ruling P16 (T2): missing speed in a frame is a gap (None) in the 1-minute trail, never 0 — spec anti-goal overrides Mac behaviour; test renamed `missing_speed_is_a_gap` — cost: trail drawing handles gaps in M4.
- Ruling P17 (T3, T7): implementers must give real assertions: TLS test asserts session established and relay round-trip with fake lockdownd; history text test asserts both locales contain the event's words for each kind — cost: none.
- Ruling P18 (T8/T11/T12, backoff): one rustls client config + `user_agent()` in `cg_tools::net`; cg-diag depends on cg-tools for it. Battery/8091 retries reuse cg-core's existing controller backoff (2,4,8,16,30 s) — no duplicated logic — cost: cg-diag pulls cg-tools.
- Ruling P19 (T2): key `phone.charging_watts` = "充电 %{watts} W" / "Charging %{watts} W", matching phone.supply — cost: none.
- Ruling P20 (T3 ⚠️ re P18): rustls client config lives in two places: `cg_core::tls` (service) and `cg_tools::net` (user processes); cg-tools does not depend on cg-core — keeps the GUI/CLI tool crate light — cost: two ~10-line builders to keep in step.
- Ruling P21 (T3 minor 1, load-bearing for T5): after any Err from a Relay/Session call the caller drops it and reopens; T5's battery loop must do this — partial-frame timeouts desync the stream — cost: none.
- Ruling P22 (T6 minor 1, spec vs brief type): `LastIssue.duration` becomes `Option<i64>` (skip if None) — spec §3 anti-goal (never fake 0) beats the brief's i64 — carried into T7 since the type is not consumed yet — cost: none.
- Ruling P23 (T6 minor 2): `HealthLog::append` truncates `t` to whole seconds itself — Mac .iso8601 can't read fractions — carried into T7 — cost: none.
- Ruling P24 (T7 minor 3): write-failure warn is once per failure streak (resets after a success) — better than once-ever when a disk flaps — cost: an extra warn line per streak.
- Ruling P25 (T8 minor 6/7): Target stays with public fields (brief); production callers must use Target::production() — T9/T11 reviewers check; user's own note is not redacted (Mac design) — cost: a user may paste personal data into the note knowingly.
- Ruling P26 (T10): the stored smoke report CG-FEX8W came from my dispatch overriding the brief's "store nothing"; deleting it from R2 is left to the user (permanent delete) — cost: one test report in the inbox.
- Ruling P27 (T12 Important 1): uplink = lowest-metric default route after filtering out cellglide0, interfaces without a device link, and phone-tethering drivers (ipheth, rndis_host, cdc_ncm, cdc_ether); a VPN owning the best default no longer forces None — bound-socket measurement over Wi‑Fi/Ethernet is still a true Wi‑Fi/Ethernet measurement — cost: on unusual setups the "comparison" leg may pick a NIC the user didn't expect.
- Ruling P28 (T14 Step 3): the test machine now runs the package (not dev install); for the device checklist the controller builds an x86 deb from linux-1.0-full HEAD in the podman builder (same version 1.0.0~beta.2, not published) and installs it with dpkg — keeps the packaged layout and unit — cost: apt shows the repo's beta.2 as equal; next published version upgrades over it.
- Ruling P29: one fix wave also takes minors: history_points gap not 0, diagnostics drops -y, doc corrections, naming-split doc comment — cheap, and P16/consent-related — cost: none.
- Ruling P30 (residual minor): NoExit counted as tunnel evidence even when phoneExit leg is ok can attribute to computerRelay — parked: a SOCKS exit check failing while the phone reports exitOK is plausibly a relay-side fault — cost: occasional mis-attribution in history; revisit with device data in M4/M5.

## 留下的小问题与暂缓事项

- Task 1: minor (deferred): is_named matches either language (untested); no test that old Snapshot without phone/labels parses; large_enum_variant allow (box if Snapshot grows); socket2 out of alphabetical order
- Task 2: minor (deferred): feed uses std Instant vs tokio Instant elsewhere; no test with charging+fully_charged+watts together
- Task 3: minor (deferred): probe continues after info() failure; fake lockdownd only negotiates TLS 1.3 (1.2 path untested — Task 4 decides); encode clones the dict; DeviceCertificate pinning as a later hardening
- Task 5: minor (deferred): label race has no test; fake mux routes by port (A/B swap test leans on battery_connections)
- Task 6: minor (deferred): no DST-day test; prune may drop the day-before file needed by events_on(today-6) after an eastward time-zone change
- Task 7: minor (deferred): no sleep/wake judge reset (Mac resets on willSleep/didWake; logind PrepareForSleep later); sync log write on actor (brief-mandated); status prints health line only when Connected; probe DNS via system resolver when Unmanaged / no happy-eyeballs; DST-day strip offsets; monitor→controller dependency for HealthWatchTiming; no weekday on older dates; relisten usbDetached implicit; block v0.1.6 future-incompat warning (cg-gui dep)
- Task 8: minor (deferred → folded into T9 dispatch): GENERIC_NAMES lacks ubuntu/debian/dev/test etc. (over-redaction); SSID_QUOTED stops at inner apostrophe; SSID_BARE \s* spans newline
- Task 9: minor (deferred, flag for final review): week range can time out and lose all lines — add `-n 2000` to journalctl; no cap on concurrent diagnostics requests (Semaphore(1)); unbounded single stdout line before 8 KiB cut; Mac parity: carrier/radio on Phone line and SIM "data on" suffix; spawn_blocking expect → error reply; login test can skip silently; doc comment on confirmed()
- Task 10: minor (deferred): UA regex shared char class lets Mac/iOS UAs contain ~ and +; no test pinning that
- Task 11: minor (deferred): CLI error paths may wait on a blocked getaddrinfo thread at exit; non-Status reply counts as NotSharing; no test of the 6 s timeout path; terse "!" ECS lines
- Task 12: minor (deferred): bridges/bonds/VLAN/PPPoE/IPv6-only not detected as uplinks (safe failure); no Happy Eyeballs (check v6 on device in T13/T14); DNS resolved before bind; ServerName error mapping
- Task 12: minor (deferred): Params::check uses Io with free text; orphan .tmp files after a crash
- Task 13: minor (deferred → folded into T14): bind-device-check.sh should assert non-root and exit nonzero on unexpected result; confirmation gate untested (extract pure fn + tests); progress line lacks leg label; wifi_unavailable shown only after phone leg; empty history leg row
- Task 13: minor (deferred): RTF_UP ignored in script; snapshot read before prompt; no hint when phone absent but Wi‑Fi runs; cloudflare_small to be covered by controller's real run
- Parked for M4 (from triage): sleep/wake judge reset via logind PrepareForSleep; GUI diagnostics preview paging; status health line when disconnected; DST strip; weekday on older dates; Mac parity carrier/radio on Phone line; ECS hint wording; Wi‑Fi-only hint; sudo diagnostics redacts only root's login.
- Parked for M5: rp_filter strict on Debian/Fedora for --compare; IPv6 Happy Eyeballs; bridge/bond/VLAN/PPPoE uplinks; DNS resolved via phone for Wi‑Fi leg; LogsDirectory instead of journal group (P14 long-term); next release must bump past 1.0.0~beta.2 (P28).
- Ruling P30 (residual minor): NoExit counted as tunnel evidence even when phoneExit leg is ok can attribute to computerRelay — parked: a SOCKS exit check failing while the phone reports exitOK is plausibly a relay-side fault — cost: occasional mis-attribution in history; revisit with device data in M4/M5.
- Parked residuals: regression test uses 30 s cadences (3 s fast path interleave untested); disconnected diagnostics waiter still builds once; eprint! on closed stderr still panics.
