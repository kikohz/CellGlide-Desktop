# Linux 1.0 M5 发布：执行记录

按子代理逐任务执行时控制者所做的判定，以及审查中留下、未在本里程碑处理的小问题。原始台账在执行完成后删除，这里是它的存档。格式：判定 — 理由 — 判错的代价。

## 判定

- Ruling P1: T2 implementer edits test-publish-local.sh but does not run it; the run happens in T5 Step 3 once T4 built both arches — dist/1.0.0 cannot exist during T2 — cost: script bug found later in T5.
- Ruling P2: T4 Step 3 uses scripts/test-publish-local.sh (throwaway key) instead of a real-key --dry-run — keeps the passphrase out of the loop until T6 — cost: none (same preflight checks).
- Ruling P3: T4, T5, T6 are executed by the controller (builds on Mac/test machine, Lima VMs, device, user-run publish); T5's doc commit gets a light review — no implementable code — cost: less independent review on validation text.
- Ruling P4 (T2): generate release-linux.json before any upload (into dist/<v>/ for real runs), so a generator failure aborts the publish before anything goes live — overrides the brief's "after published-versions" — cost: none; the file just exists slightly earlier.
- Ruling P5 (T3): keep the two README-derived sentences (Debian needs systemd-resolved; log out once after first install for group membership) — they are true and prevent support requests — cost: slightly longer section.
- Ruling P6 (T3): update copy that becomes wrong once Linux is live: download page title "for Mac", FAQ "需要准备什么" (Mac only), footer system requirements; leave the "OpenWrt 路由器版现已推出" badge — spec says the site must present Linux as downloadable — cost: a few more strings in three languages.
- Ruling P7: run the final whole-branch review before Task 6 publishes — a defect found after publish costs a new release — cost: packages must be rebuilt and smoke re-run if the fix wave changes shipped code.
- Final review minors parked: Minor 8 (Debian systemd-resolved note vs "doesn't change your network" FAQ) — Ruling: leave; the user installs the system package themselves — cost: a reader may see slight tension. Minor 9 (no backup step for signed.noindex/1.0.0) — controller backs it up in T6 after publish.

## 留下的小问题

- Task 1: minor (deferred): see task-1-review.md Minor 1–5 (rustfmt noise in Msg::Progress, README reason line, synchronous history read at startup, colima left running, build.noindex untracked)
- Task 2: minor (deferred): date changes on a rerun of the generator (Minor 6); no CLI exit-code test
- Task 3: minor (deferred): remaining minors in task-3-review.md; changelog bullets to be checked against the running 1.0.0 app (T5 device smoke); provisional date 2026-10-21 set until T6
- Final review (pre-publish) minors 1–7 and 10: see the review notes; none blocked publishing. Minor 8 parked (Debian systemd-resolved note), minor 9 handled (signed.noindex/1.0.0 kept locally).
