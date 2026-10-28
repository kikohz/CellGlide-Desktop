# Linux 1.0 M2 权限与安装包：执行记录

按子代理逐任务执行时控制者所做的判定，以及审查中留下、未在本里程碑处理的小问题。原始台账在执行完成后删除，这里是它的存档。格式：判定 — 理由 — 判错的代价。

## 判定

- Ruling: polkit rules are REQUIRED package contents (both JS 50-cellglide.rules and cellglide.pkla), not optional — measured denial on all 6 — cost if wrong: two small extra files.
- Ruling: deb `Recommends: polkitd | policykit-1`, rpm `Recommends: polkit` — every desktop already has it; headless pulls a small daemon — cost if wrong: an extra package on minimal servers.
- Ruling: Task 3 maps resolved errors by D-Bus error NAME (org.freedesktop.DBus.Error.AccessDenied, ...InteractiveAuthorizationRequired) → ApplyError::NotAuthorized → DnsMode::Unmanaged with hint; message text never parsed — Ubuntu 26.04 messages differ — cost if wrong: an unknown error name shows as Failed instead of NotAuthorized.
- Ruling: Unmanaged text now says websites won't open without another network — reviewer product note; plan text was misleading for the no-other-network case — cost if wrong: copy tweak.
- Ruling: controller implemented Steps 2–4 directly (export + two verbatim config files + doc) — purely mechanical, verified exported files hold no secret key — cost if wrong: doc wording.
- Ruling: apt keyring moves to /etc/apt/keyrings (conffile with .sources; survives remove, purge clears both) — remove→update broke otherwise — cost: rebuild all packages, deviation from constraint path.
- Ruling: run final whole-branch review now, before publishing — publishing is outward-facing; fixes are cheaper before — cost: one more review later if Task 8 Steps 5–6 change code.
- Ruling: resolved missing (ServiceUnknown/NameHasNoOwner) → DnsMode::Unmanaged, not Failed — same meaning (tunnel works, DNS not ours) and stops the retry loop — cost: users without resolved get a working tunnel with system DNS instead of an error.
- Ruling: Unmanaged text points to `cellglide doctor`; doctor gains resolved-running, polkit-present, rule-file-present checks with fix commands — spec §3.5 asked for doctor fix commands — cost: a few i18n strings.
- Ruling: upgrade restarts only if enabled (deb+rpm) — debhelper convention.
- Ruling: previously published versions verified by recorded sha256 (committed alongside published-versions), only current version vs packaging/ — key renewal must not block publishing.
- Ruling: one fix wave also takes cheap minors: refuse re-signing an already-published rpm with no signed copy; drop SupplementaryGroups=systemd-journal (diagnostics not built); rpm drops pkla; stronger wrong-key assert; README PackageKit group note; install-dev --uninstall must not delete package-owned files.
- Ruling: ship the tray fix as 1.0.0~beta.2 (published bytes of beta.1 are immutable) — also exercises the real apt upgrade path on the test machine — cost: one more publish (user passphrase).

## 留下的小问题与暂缓事项

- Parked minors: gateway_notice stringly-typed ("missing"/"back") with `_ =>` fallthrough (brief prescribed signature); redundant `if let` in the "back" arm.
- Parked minor: Debian stock images' polkit presence not checked.
- Parked: resolver binds 1053 before knowing DNS is unmanaged (idle resolver; ResolverPortBusy could fail an otherwise-unmanaged connect); NotAuthorized revert is a no-op; real hardened unit not yet run end to end (→ Task 7 install tests); 0.105 pkla branch only read, not run (→ Task 7 ubuntu-22.04); doctor lacks a polkit check (later).
- Parked: GUI runtime libs (wayland/x11/vulkan, dlopen'd) not in Depends — desktops have them, check on Task 7; rpm Requires list not re-inspected; postinst branches untested until Task 7; stale /usr/local/bin binary if dev unit hand-edited.
- Parked: duplicate libc6 entries in Depends (cosmetic); RUST_VERSION pin hand-maintained.
- Parked: other five not re-run with asserting script; assert failure paths not forced; no-resolved distro now fails check a (script, not product); rpm leaves /var/lib/cellglide and both keep the cellglide user (by design); GUI runtime libs absent on server images (informational).
- Parked: upload window between Packages and InRelease (Acquire-By-Hash) — beta acceptable; dev-unit-over-package disables on next upgrade (rare); hardening directives.
- Parked from fix report: dnf5 skips repos with bad signatures (exit 0) — user-facing note later; URL-install of rpm unverified on first install (HTTPS only); new-fingerprint key rotation blocks publish (renewal fine); resolved-missing not tested end to end.
- Parked: N4 (Unavailable not retried within a connection; doctor may show all ✓), N5 (unreadable paths count as present), N6 (CLI shows protocol 1 on parse error). Rule: after protocol release, new CheckIds need a version bump or serde(other).
- Residual fixes e0e0760 (cross-arch check in check-package-conffiles.sh; DEV_FIRST adds user autostart file; 26.04 DEV_FIRST PASS) and 201ed7d (revert on Unavailable). Controller accepted; final review loop closed — residual minors parked above.
- Tray fix 12e9712; review (opus) Needs fixes (manual launch auto-closed window; offline/TrayReady race; ksni subscription race). Fix round 1 35dceb8 (--background autostart, Mutex ordering, NameHasOwner watchdog ≤5 respawns) + 3d216c2 version 1.0.0-beta.2. Re-review: all addressed; new Critical TryExec contained "--background" (would make autostart skip the entry) — controller fixed c781eaf with activate() clone. Parked: watchdog respawn counter never resets; stale bus after session-bus restart; ksni online-before-register flash.
