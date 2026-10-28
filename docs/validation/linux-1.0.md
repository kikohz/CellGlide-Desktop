# Linux 1.0 验证记录

## M1 网络稳定

日期：2026-10-15
测试机：<build-host>（x86_64 一体机，Ubuntu 26.04 x86_64）
iPhone：iPhone Air（Verizon 5G NSA），手机 CellGlide 2.2.0；Air 开着「断开 USB 后自动停止」
构建：linux-1.0 @ b21131e

### 自动测试

| 环境 | 命令 | 结果 |
|---|---|---|
| macOS | `cargo test --workspace` | 117 通过，1 忽略 |
| Docker | `scripts/linux-test.sh` | 118 通过，12 忽略 |
| Lima（宿主） | 特权测试 | 11 通过 |
| Lima（netns） | `scripts/bypass-netns-test.sh` | 7 通过 |
| 联网 | `cargo test -p cg-core exit_check -- --ignored` | 1 通过 |

### 实机清单（1–11）

| # | 项目 | 结果 |
|---|---|---|
| 1 | 出口 IP 为蜂窝 IP | ✅ 插上即自动连接（未执行任何命令），出口 Verizon IPv6，`1.1.1.1` 走 cellglide0 |
| 2 | DNS 不泄漏 | ✅ 解析出口 162.244.53.x（Air 上配置的 DoH 服务商），查询期间 Wi‑Fi 网卡无 53/853 包 |
| 3 | UDP 可用 | ✅ `dig @1.1.1.1` 正常 |
| 4 | 拔线后网络自动恢复 | ✅ 拔线后 0.1 秒内拆除隧道与绕过路由，回到 Wi‑Fi |
| 5 | `kill -9` 不留痕迹（含绕过路由：重启服务后清理） | ✅ 网卡、半路由、1053 立即消失；6 条绕过路由在 systemd 重启服务时被清理（自动连接关闭时验证，避免重连干扰）；快照与基线一致 |
| 6 | 其他网卡 DNS 前后一致 | ✅ 其他网卡 DNS 与路由一致。resolv.conf 的 search 行两个域换了顺序，断开 CellGlide 后仍是新顺序，系 Tailscale 重新登记所致，与本程序无关。快照脚本尚未排除自己的 realm 198 路由（M2 修） |
| 7 | 插回自动重连 | ✅ 电脑端插回后持续重试；Air 因「断开 USB 后自动停止」需在手机上重新开始共享，点后 1 秒内自动连上 |
| 8 | 手机关掉 CellGlide DNS | ✅ 约 1 秒自动重建，改用 4 个公共 DNS，1053 关闭；Wi‑Fi 上只见系统反查本机名的 PTR（resolved 对本网段反查的正常路由） |
| 9 | 界面 | ✅ 中文窗口显示已连接与 Verizon；托盘菜单、关窗重开、断开再连接正常 |
| 10 | 连接期间局域网设备发现正常（`avahi-browse -art` 能看到局域网服务；SSDP 不进 cellglide0） | ✅ 6 条绕过路由经 192.168.0.1 / wlp2s0（metric 32766）；`239.255.255.250` 走 wlp2s0；cellglide0 上抓不到组播包。该局域网里 avahi 没发现服务，未能用实际设备验证发现功能 |
| 11 | 电脑重启后自动重新连接 | ✅ 开机后服务启动几秒内自动连上（这次 Air 未停止共享）。开机时 Wi‑Fi 晚于本服务就绪，绕过路由在 Wi‑Fi 起来后自动补上 |

### 健康检查

| 项目 | 做法 | 结果 |
|---|---|---|
| 手机开飞行模式后只开 Wi‑Fi（蜂窝断） | 连接中操作，看多久释放、状态显示什么 | ✅ 开飞行模式（Wi‑Fi 关）后约 10 秒释放（手机上报断网触发提前检查，首次失败后 5 秒重试再失败），回到 Wi‑Fi，状态为「手机网络暂时不通」 |
| 恢复蜂窝 | 是否按退避自动重连 | ✅ 关飞行模式后自动重连，无需操作 |
| 手机上的「已连接设备」行 | 是否显示 USB 速率 | ⚠️ 电脑端读到 Air 的 sysfs 序列号为去掉连字符的 UDID、速率 480 Mbit/s 并上报；手机上的显示待确认 |
| 中英文 | `LANG=en_US.UTF-8 cellglide status` 与 `LANG=zh_CN.UTF-8 cellglide status` | ✅ 中英文都正常 |
| 设置 | `cellglide settings --auto-connect off`、重启服务后仍为关 | ✅ 重启服务后仍为关，settings.json 内容正确 |

### 问题

- 开机时 Wi‑Fi 尚未就绪，「no default route outside cellglide0」警告在几秒内打印了 20 次，应只在状态变化时打一次（M2）。
- 快照脚本应排除本程序自己的 realm 198 路由（M2）。
- 插回后手机端恢复：Air 开着「断开 USB 后自动停止」时按设计停止共享；16e（未开此选项）插回约 15 秒恢复。均为 iOS 端行为，由用户在 iOS 侧处理。
- M1 的「日常使用一周无需手动干预」出口条件由用户在测试机上继续观察。

## M2 权限与安装包

日期：2026-10-15
方法：`scripts/test-install-vm.sh`，每个发行版一台全新的 Lima VM（arm64，测完即删），安装 `1.0.0~beta.1`，升级到 `1.0.0~beta.1.1`（临时 worktree 构建，未提交），再卸载（deb 用 purge，rpm 用 remove）。安装通过 `limactl shell … sudo`，所以 `SUDO_USER` 是 Lima 的普通用户，安装时把它加入了 `cellglide` 组并提示重新登录。DNS 与 resolved 授权的结论见 `linux-1.0-resolved-auth.md`。

> 注：apt 公钥环的包内路径由 `/usr/share/keyrings/cellglide-archive-keyring.gpg` 改为 `/etc/apt/keyrings/cellglide-archive-keyring.gpg`（计划里的约束写的是旧路径）。`.sources` 是 conffile，`apt remove` 后保留；公钥环放在 `/usr/share` 会被 remove 删掉，之后每次 `apt update` 都报错，放在 `/etc` 则同为 conffile，remove 保留、purge 才一起删。下面「安装、升级、卸载」与「附加检查」两张表是改路径前的测试结果；用最终安装包重测的结果见文末「最终修正后的复测」，以那一节为准。

### 安装、升级、卸载

| 发行版 | 安装 | 服务身份 | 升级保留设置 | 卸载干净 | 网络不变 |
|---|---|---|---|---|---|
| Ubuntu 22.04 | ✅ | ✅ User=cellglide，cap_net_admin | ✅ | ✅ | ✅ |
| Ubuntu 24.04 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Ubuntu 26.04 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Debian 12 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Debian 13 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Fedora 42（rpm） | ✅ | ✅ | ✅ | ✅（`/var/lib/cellglide` 保留，见下） | ✅ |

### 附加检查（a–e）

| 发行版 | a polkit 授权（SetLinkDNSEx） | b realm 198 清理 | c `ldd` 缺库 | d 安装/升级后 is-enabled | polkit | resolved |
|---|---|---|---|---|---|---|
| Ubuntu 22.04 | allowed（pkla 路径） | 退出 0（iproute2 5.15） | 无 | enabled / enabled | 0.105 | 已在运行 |
| Ubuntu 24.04 | allowed | 退出 0（6.1） | 无 | enabled / enabled | 124 | 已在运行 |
| Ubuntu 26.04 | allowed | 退出 0（6.19） | 无 | enabled / enabled | 127 | 已在运行 |
| Debian 12 | allowed | 退出 0（6.1） | 无 | enabled / enabled | 122 | 已在运行 |
| Debian 13 | allowed | 退出 0（6.15） | 无 | enabled / enabled | 126 | 已在运行 |
| Fedora 42 | allowed | 退出 0（6.12） | 无 | enabled / enabled | 126 | 已在运行 |

- a：服务用户 `cellglide` 通过 `systemd-run` 对虚拟网卡 `cgtest0` 调用 `SetLinkDNSEx`，六个发行版都是 allowed；测完 RevertLink 并删除 `cgtest0`，没碰真实网卡。本次的 Lima Debian 镜像已自带并运行 systemd-resolved，所以没有出现「无 resolved」的情形，脚本里的「装上 resolved 再测一次」分支未触发；polkit 在六个镜像里都存在（镜像本身已带 polkit）。
- c：缺失的运行时库（`ldd` 本身都齐全）。GUI 运行时才加载的库在服务器镜像上：`libwayland-client.so.0` 全部缺失；`libxkbcommon.so.0` 在 Debian 12/13、Ubuntu 22.04 缺失，其余有；`libX11.so.6` 在 Debian 12/13、Fedora 42 缺失，其余有。仅供参考，服务器镜像本来没有桌面。
- d：Ubuntu 24.04 上 `apt-get remove` 后单元文件消失（`not-found`，服务 inactive），重装同一个 deb 后回到 enabled，符合预期。
- e：`ubuntu:22.04` 容器（无 systemd，`--network host`）里 `apt-get install` 该 deb 退出 0。
- 升级后设置保留（`settings kept`）与卸载后 `clean`、`network unchanged` 六个都输出。

### 观察

- deb purge 后 `/var/lib/cellglide` 被删，但 `cellglide` 系统用户仍在（由 sysusers 创建）。rpm remove 后 `/var/lib/cellglide` 保留。

### 修正与未测项（Fix round 1）

- 脚本改为带断言：服务身份、a、b、d、e 和网络快照非空任一不满足就输出 `FAIL <项>` 并非零退出；c 仅供参考。已用断言版重跑 Ubuntu 22.04（deb 加升级 deb）：全部通过。
- Ubuntu 22.04 的 polkit 证据（重跑）：`pkaction` 为 0.105，`/var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla`（276 字节）与 `/usr/share/polkit-1/rules.d/50-cellglide.rules`（478 字节）两个文件都已安装，a 为 allowed。0.105 只读 pkla，所以这次授权走的是 pkla；JS 规则在 0.105 上被忽略。
- 未测：无 `SUDO_USER`（没有桌面用户）的安装。Lima 的 sudo 会设置 `SUDO_USER`，所以这条路径没有覆盖。
- 未测：rpm 的 remove 后重装循环、无 systemd 的 rpm 容器安装（只测了 deb 的这两项）。
- 升级测试时隧道从未建立（没有接手机），所以「先停旧服务再替换」只在 Task 8 的真机上验证。

## M2 最终修正后的复测

日期：2026-10-15。安装包：arm64 `1.0.0~beta.1` 与 `1.0.0~beta.1.1`，都由 3fae493（含本轮全部代码与打包改动）在 Mac 上用 `scripts/build-packages.sh` 构建；`1.0.0~beta.1.1` 只是临时把工作区版本改为 `1.0.0-beta.1.1` 构建，未提交。x86_64 未重建。每个发行版一台全新的 Lima VM，测完即删。下面每一项都是脚本里的断言（不满足就输出 `FAIL` 并非零退出），不是人工看日志。

### `scripts/test-install-vm.sh`

| 发行版 | 方式 | 结果 |
|---|---|---|
| Ubuntu 22.04 | `DEV_FIRST=1 CYCLE=1`，deb beta.1 → beta.1.1 | PASS |
| Ubuntu 24.04 | `DEV_FIRST=1 CYCLE=1`，deb beta.1 → beta.1.1 | PASS |
| Debian 12 | `CYCLE=1`，deb beta.1 → beta.1.1 | PASS |
| Fedora 42 | rpm beta.1 → beta.1.1 | PASS |

每次运行都断言：服务身份（`User=cellglide`、cap_net_admin），a（polkit 允许服务用户对 `cgtest0` 调用 `SetLinkDNSEx`），b（realm 198 清理退出 0），d（安装、升级后 enabled；deb 的 remove 后重装回到 enabled），f，卸载后服务停止、无 `cellglide0`、无 realm 198 路由，网络快照前后一致且非空。c 仅供参考。

- **g：从 M1 开发版升级到软件包**（22.04 与 24.04）。先在 VM 里复原 M1 状态：只有 `cellglide` 组（GID 900，不是 groupadd 下一个会分配的 998 / 988）、没有用户；`/etc/systemd/system/cellglide.service` 取自 be1fc48（root 运行，`/usr/local/bin/cellglide-service`），三个占位程序，已 enable 并在运行；root 所有的 `settings.json`，`auto_connect` 为 false；开发版 polkit 文件（22.04 为 `/etc/polkit-1/localauthority/50-local.d/cellglide.pkla`，24.04 为 `/etc/polkit-1/rules.d/50-cellglide.rules`）。装 deb 后断言：`User=cellglide`、ExecStart 为 `/usr/bin/cellglide-service`；`/usr/local/bin/cellglide*`、开发版 unit 与 polkit 文件都已删除；enabled 且 active；设置保留，`/var/lib/cellglide` 与 `settings.json` 属于 `cellglide`；`id cellglide` 为 `uid=900 gid=900`，即 sysusers 复用了已有组的 GID。
- **f：升级只在已启用时重启服务**（四个发行版）。`systemctl disable --now` 后降回 beta.1（只为回到旧版本），再升级到 beta.1.1：仍为 inactive / disabled。之后 `enable --now` 并重装 beta.1.1（deb 用 `dpkg -i`，rpm 用 `dnf reinstall`，走的是升级分支）：服务重新 active。
- `cellglide doctor` 在 22.04（polkit 0.105）上显示 polkit 已安装、CellGlide 的 polkit 规则已安装（包里 `/usr/share` 下的 JS 规则文件）。
- Fedora 42 的 rpm 不再带 pkla，只有 `/usr/share/polkit-1/rules.d/50-cellglide.rules`，a 仍为 allowed。

### `scripts/test-publish-local.sh`（一次性密钥，不上传）

- 已发布版本按字节核对：用 `1.0.0~beta.1.1` 模拟「已经发布过的版本」（rpm 用一次性密钥签好，记录写进临时的 `published-versions` / `published.sha256`）。记录为空、记录与文件不符、签名副本缺失三种情况都以非零退出并给出对应错误，且没有签任何东西；记录正确时打印 `published bytes ok`，复用已签的 rpm（字节不变），当前版本照常检查 conffile。dry run 打印当前版本将追加的四行记录。
- apt 错误公钥：`apt-get update` 退出码非零，且有 `NO_PUBKEY`。
- **apt 不用 `force-confold`**（Ubuntu 24.04）：purge 之后删掉 `force-confold`，确认 dpkg 与 apt 都没有 conffile 相关选项；按 `setup-apt.sh` 在生产环境的做法预先放好与包内逐字节相同的 `/etc/apt/keyrings/cellglide-archive-keyring.gpg`（`packaging/keys`）与 `/etc/apt/sources.list.d/cellglide.sources`（`packaging/repo`，指向 `https://pkg.cellglide.com/apt`）；VM 的 `/etc/hosts` 把 `pkg.cellglide.com` 指向 127.0.0.1，所以生产地址只在 VM 内连接失败（Ign），不出 VM。包从另一个指向本地仓库、用一次性公钥的源安装，stdin 为 `/dev/null`。断言：退出 0，安装日志里没有 `Configuration file` / `conffile` 字样，两个文件仍与原文件相同。
- dnf 错误公钥：Fedora 42 的 dnf5 默认会跳过元数据验签失败的仓库并以 0 退出（只打印 Librepo 的签名错误）；脚本因此用 `skip_if_unavailable=False` 断言非零退出，再检查签名错误字样。
- **dnf 先按 URL 安装再升级**（Fedora 42）：`dnf install http://host.lima.internal:…/rpm/aarch64/cellglide-1.0.0~beta.1-1.aarch64.rpm` 成功；然后 `dnf upgrade --refresh` 走包里自带的 `cellglide.repo`（`gpgcheck=1`、`repo_gpgcheck=1` 不变，只用 `--setopt` 把 baseurl 和 gpgkey 指向本地仓库与一次性公钥）升到 `1.0.0~beta.1.1`，签名为一次性密钥，服务 active。

### 仍未覆盖

- x86_64 包没有用本轮代码重建和复测（由构建机另行完成）。
- 无 `SUDO_USER` 的安装（如 PackageKit）只在 README 里说明需要手动加组，没有实测。
- resolved 缺失：D-Bus 错误名在 debian:12 容器（有 system bus、无 resolved）里测得为 `org.freedesktop.DBus.Error.ServiceUnknown`，单元测试覆盖分类；没有在装有 CellGlide 的机器上卸掉 resolved 实测隧道（会改动那台机器的 DNS）。

## M2 发布与实机复测

日期：2026-10-16
测试机：Ubuntu 26.04.1 x86_64（同 M1），iPhone（AT&T 5G NSA，手机 CellGlide 2.2.0）

### 发布

- `1.0.0~beta.1`、`1.0.0~beta.2` 已发布到 `https://pkg.cellglide.com`（apt + rpm，签名钥匙 `71082C14C8979B6C13E1B1D728A16C8C0A0D2A98`）。发布 beta.2 时脚本核对了 beta.1 已发布字节未变，beta.1 的 rpm 复用已签副本。
- 真实域名冒烟（临时 Lima VM）：Ubuntu 24.04 `curl -fsSL https://pkg.cellglide.com/setup-apt.sh | sudo sh` 退出 0，无 conffile 提问，`apt update` 无 W/E，来源 pkg.cellglide.com，服务 active、`User=cellglide`；Fedora 42 按 URL 安装后 `dnf upgrade --refresh`（`repo_gpgcheck=1`）成功，线上 rpm 用发布的公钥验签为 `signatures OK`。

### 测试机：开发安装 → 正式包

- 用一键脚本安装 beta.1：无 conffile 提问；服务改为 `/usr/lib/systemd/system/cellglide.service`、`User=cellglide`、enabled 且 active；`/usr/local/bin` 下开发版与 `/etc/systemd/system` 开发单元已删；`cellglide` 用户 uid 972 复用原有组 gid 973（组员 root、x 保留）；`settings.json` 内容不变、属主改为 cellglide；`cellglide doctor` 除「未插 iPhone」外全部 ✓；网络快照前后一致。用户 x 的旧自启文件（指向 `/usr/local/bin/cellglide-gui`）经用户同意删除。
- 之后 `apt-get install --only-upgrade cellglide` 升到 beta.2：无提问，服务保持 enabled/active，设置不变，网络快照一致。

### 实机清单（正式包，专用用户）

| # | 项目 | 结果 |
|---|---|---|
| 1 | 出口 IP 为蜂窝 IP | ✅ `1.1.1.1` 走 cellglide0，出口 AT&T IPv6 |
| 2 | DNS 不泄漏、由 CellGlide 接管 | ✅ `resolvectl dns cellglide0` = `127.0.0.1:1053`、域 `~.`（polkit 规则在真机生效）；查询期间 Wi‑Fi 无 53/853 包 |
| 4 | 拔线后网络自动恢复 | ✅ 下一次 0.5 秒采样内 cellglide0 与 6 条绕过路由拆除，回到 wlp2s0 |
| 7 | 插回自动重连 | ✅ 约 15 秒后自动连上，DNS 重新接管 |
| 9 | 界面与托盘 | ✅（beta.2）重启后以 `cellglide-gui --background` 自启，托盘项在 GNOME 注册，图标可打开窗口、菜单可用。beta.1 在开机时托盘服务未就绪就放弃托盘，只剩窗口，beta.2 已修 |
| 11 | 重启后自动连接 | ✅ 开机约 10 秒连上；Wi‑Fi 晚 3 秒就绪，只打一次「没有默认路由」警告，随后绕过路由自动补上 |

### 下个版本一起做（用户决定）

- Dock 栏显示的图标不对：GNOME Wayland 按窗口 app_id 匹配 `cellglide.desktop`，窗口未设置 application_id。
- 界面里看不到版本号：窗口、托盘与 `--version` 都要显示。

## M3 数据与服务

日期：2026-10-16
构建：linux-1.0-full @ 31e105b（含本节之前的全部 M3 代码与清理）
测试机清单 a–h 由控制者在 `root@<test-machine>`（接 iPhone 16e）上执行；执行者未碰该机，也没有跑真实测速。

### 自动测试

| 环境 | 命令 | 结果 |
|---|---|---|
| macOS | `cargo test --workspace` | 379 通过，1 忽略 |
| Docker | `scripts/linux-test.sh` | 383 通过，15 忽略（特权测试 12、联网测试 3） |
| Lima（宿主） | 特权测试（`--ignored --test-threads=1`） | 12 通过 |
| Lima（netns） | `scripts/bypass-netns-test.sh` | 8 通过 |
| 诊断服务 | `npm test`（CellProxy 的 `diag-linux` 工作树，`Diagnostics/worker`） | 12 通过 |

- 文案完整性：`cg-i18n` 的 `every_key_exists_in_both_languages_with_the_same_placeholders` 覆盖全部新键；`Reason`、`IssueCause`、`EventKind` 的键由 `cg-ipc` 的 `reason_and_cause_keys_are_listed` 逐个核对，泄漏结论的键由 `cg-tools` 的 `every_verdict_has_text_in_both_languages` 核对，`SpeedError` 的键由 `cg-cli` 的 `every_error_has_text_without_placeholders` 核对（映射函数在 `cg-cli`，所以测试放在那里）。每个测试都检查键在 `KEYS` 中，且中英文输出里没有键名或未填的占位符。

### SO_BINDTODEVICE（非 root 绑定网卡，Task 13）

`scripts/bind-device-check.sh`：绑定默认路由网卡后向 1.1.1.1:443 建立一次 TCP 连接（不传数据），再确认不能改绑到 `lo`。脚本现在拒绝以 root 运行（退出 2），结果行不是预期值时退出非零。

| 内核 | 环境 | 结果 |
|---|---|---|
| 6.8.0-134 | Lima `cellglide-test`（Ubuntu 24.04，由模板 ubuntu-24.04 重建） | `RESULT: unprivileged bind ok, rebind refused`；以 root 运行退出 2 |
| 5.15.0-185 | 临时 Lima（Ubuntu 22.04，测完即删） | `RESULT: unprivileged bind ok, rebind refused`（该行在加 root 防护之前跑的；root 防护的退出 2 只在 6.8 上验证过） |

测速引擎的单元测试 `bound_to_loopback_measures_against_the_fake` 以普通用户绑定 `lo` 通过。联网的 `cloudflare_latency_only`（3 个极小请求，绑定 eth0）通过，0.42 秒；25 MB 的 `cloudflare_small` 和完整测速未跑，留给控制者。Lima 没有全局 IPv6，AAAA 排序与 Happy Eyeballs 没能观察，见下「留到 M4」。

### DNS 泄漏真实服务器验证（Task 11）

- 没有改动 VPS。Mac 上用 `-A CellGlide-Linux/1.0.0` 访问泄漏检测服务得到 HTTP/2 200 与带 resolvers 的 JSON，Cloudflare 没有拦 Linux 的 UA。
- Mac 上跑引擎：拿到报告，结论「没有经过手机」，出口 T-Mobile，解析器 Zenlayer。
- Lima 里 `sudo cellglide dns-leak`（无手机）：中英文都输出「这台电脑没有使用手机的网络」，退出 0。
- 手机在线时的结论由清单 f 验证。

### 诊断服务部署（Task 10）

- 代码在 CellProxy 的 `diag-linux` 工作树（提交 03e1ad6，未推送、未合并，由用户决定）：UA 允许 `CellGlide-Linux/…`，平台白名单加入 `linux`，Mac 与 iOS 上传行为不变。
- 部署用项目自带的 wrangler 4.149.0，现行版本 `5edbc0a9-7e9d-4925-bcdd-7d5b2a603a9d`，绑定为 REPORTS 与 UPLOADS（每 60 秒 3 次）。
- **事故**：第一次部署在 `npm ci` 还没装完时开始，npx 退回 wrangler 4.31.0，它不认识 `ratelimits`，部署时丢掉了 UPLOADS 绑定，线上所有上传（含 Mac 与 iOS）返回 500（1101），持续约 2 分钟（坏版本 `b219df60-9870-4a6d-85b4-1107d1a71733`）。用项目自带的 wrangler 重新部署后恢复，控制者确认 Mac、iOS、Linux 的 UA 对无效请求都返回 400。教训：必须在 `npm ci` 完成后用项目自己的 wrangler 部署。
- 生产冒烟：Mac UA 加非 JSON 返回 400；Linux UA 加合法报告返回 200，报告号 CG-FEX8W（一份测试报告留在 R2，需用户自行删除）；Windows UA 返回 403。
- 真实 Linux 客户端上传由清单 e 验证。

### 手机电池探测（Task 4）

结论见 [linux-1.0-m3-phone-probe.md](linux-1.0-m3-phone-probe.md)：服务用户 `cellglide` 能读配对记录、升级 TLS、打开 diagnostics relay 并读到电池字典；解锁与锁屏两种状态由控制者各跑一次，结果相同；不需要改 iOS。

### 测试机清单（a–h）

日期：2026-10-19。测试机 Ubuntu 26.04.1 x86_64（内核 7.0.0-38），装的是 `linux-1.0-full` @ 8db3f50 在 podman 构建容器里打出的 x86 deb（版本号仍为 1.0.0~beta.2，未发布）；手机 iPhone 16e、iOS 26.6、Cricket（AT&T），手机 CellGlide 2.3.0。CLI 以桌面用户 `x` 运行。

先停服务、确认 `cellglide0` 已消失再替换程序，前后各拍网络快照：

```bash
ssh root@<test-machine> 'cd /root/CellGlide-Desktop && scripts/network-snapshot.sh > /root/m3-before.txt && systemctl stop cellglide && ! ip link show cellglide0 && scripts/install-dev.sh && systemctl start cellglide'
```

| # | 项目 | 做法（桌面用户 `x`） | 通过标准 | 结果 |
|---|---|---|---|---|
| a | 完整手机卡片 | `cellglide status` | 运营商（含副品牌）、制式、速度、质量、延迟、丢包、本次用量、电量与充电；读得到的电池细节都出现，读不到的不出现 | ✅ `Cricket · 5G NSA · iPhone 16e · 80%`、速度行、`网络质量 优 · 66 ms · 本次 64 KB`、`已接电源，未充电 · 电池 38°C · 电池健康 100% · 2 次循环`；丢包为 0 时不显示，未充电时不显示功率 |
| b | 电池回报 | 手机 CellGlide 的充电卡片 | 显示电脑报来的充电功率 | ✅ 手机上显示「已连接电源」，与 Task 4 一致：电脑 USB 口约 3 W，手机未在充电，所以没有充电功率可报（与 Mac 插同类口时相同） |
| c | 连接记录 | 拔线、插回、开关飞行模式各一次后 `cellglide history` | 有 usbDetached / usbAttached / tunnelStopped / tunnelStarted 与一次 phoneNetwork 问题；`/var/lib/cellglide/health/` 属主 `cellglide` | ✅ 拔线、插回记下 tunnelStopped / usbDetached / usbAttached / tunnelStarted；飞行模式（连接中）记下一次问题，4 秒后开始、12 秒后结束并释放连接，随后自动重连；文件属主 972:973（cellglide）。⚠️ 这次的原因记成 `phoneUnstable`（「手机网络不稳」），应为 `phoneNetwork`：Linux 在 12 秒内释放会话，判定还没升级。已在终审修复中改正：出口检查失败也算作隧道失败，问题开着时隧道每 3 秒探测一次，所以现在依次记下 phoneUnstable 开始 → 升级为 phoneNetwork → 以 phoneNetwork 结束，随后的 tunnelStopped 带 `released noExit`。这由单元测试 `airplane_mode_release_is_logged_as_phone_network` 覆盖，尚未在真机上重跑 |
| d | 诊断导出 | `cellglide diagnostics --save /tmp/d.json` | `platform: linux`，`log` 非空，看不到主机名、用户名、完整 IP | ✅ `platform linux`、`format 1`，日志 18 行，环境 9 项（含手机型号、iOS、SIM、各项检查、DNS 模式）；主机名、`/home/x`、局域网 IP、Tailscale IP 都搜不到；文件权限 0600 |
| e | 诊断发送 | `cellglide diagnostics --send --note "M3 test"` | 返回 `CG-XXXXX`；R2 里 `customMetadata.platform == linux` | ✅ 返回 CG-J9U6G；R2 `cellglide-diagnostics/reports/CG-J9U6G.json` 的 `platform` 为 linux，日志 24 行，备注正确 |
| f | DNS 泄漏 | `cellglide dns-leak` | 「没有泄漏」或「用的是运营商的 DNS」，与手机 DNS 设置一致 | ✅「没有泄漏」：经 Cloudflare 加密 DNS 发出，出口 AT&T，与手机 DNS 设置一致 |
| g | 测速（一次，约 1.3 GB） | `cellglide speedtest --compare` | 两条线路都有结果与结论；`ip route`、`resolvectl` 不变；Wi‑Fi 流量出现在 `wlp2s0` 计数上 | ✅ iPhone：延迟 117 ms、下载 358.9 Mbps、上传 21.5 Mbps；Wi‑Fi（wlp2s0）：延迟 66 ms、下载 205.4 Mbps、上传 14.5 Mbps；结论「iPhone 下载比 Wi‑Fi 快 1.7 倍」。测速前后 `ip route show table all` 与 `resolvectl status` 完全相同；wlp2s0 收包 +258 MB，cellglide0 +425 MB，两条线路各走各的网卡。`--history` 记下运营商与制式 |
| h | 网络不变 | `scripts/network-snapshot.sh > /root/m3-after.txt; diff /root/m3-before.txt /root/m3-after.txt` | 无差异 | ✅ 与 CellGlide 无关的差异：两次快照之间测试机重启过（每天 01:00），tailscale0 的链路序号变了；插着手机时多出 iPhone 的 USB 网卡 `enx…`（ipheth，DOWN，无载波、无 DNS）；`/etc/resolv.conf` 仅 search 行顺序变化（Tailscale，M1 已见过）。没有任何 cellglide0 以外网卡的 DNS 或路由被改 |

### 问题与留到 M4

- 睡眠与唤醒时没有重置健康判定（Mac 在 willSleep/didWake 重置，Linux 可接 logind 的 PrepareForSleep）。
- `cellglide status` 只在已连接时显示健康行；更早的日期历史没有星期；夏令时当天的色条偏移没有测试。
- 手机行缺 Mac 上的运营商与制式、SIM「数据开」后缀。
- 速度对比：有 Wi‑Fi 但没接手机时没有提示；网桥、bond、VLAN、PPPoE、仅 IPv6 的网络识别不到（安全失败）；没有 Happy Eyeballs，路由可达但被黑洞的 IPv6 地址最多多等 4 秒（需要在有 IPv6 的真机上看）。
- 诊断：此前列在这里的四项都已处理。通用名单补上 ubuntu / debian / dev / test、SSID 脱敏处理撇号与跨行，已在 Task 9 修好；周范围的 journalctl 加 `-n 2000`、诊断请求一次只建一份（`Semaphore(1)`，第二个请求排队等候），已在终审修复中完成。
- 值得 M4 的 GUI 一并处理：Dock 图标与版本号显示（M2 遗留）。

## M4 界面

日期：2026-10-19～20。测试机 Ubuntu 26.04.1 x86_64（内核 7.0.0-38，GNOME Wayland），装的是 `linux-1.0-full` @ b68fed4 在 podman 构建容器里打出的 x86 deb（版本号仍为 1.0.0~beta.2，未发布）；修复睡眠唤醒问题后换成 f792d57 的包，唤醒钩子再换成 ce3bc65 的版本。手机 iPhone 16e、iOS 26.6、Cricket（AT&T），手机 CellGlide 2.4.0。

### 自动测试

| 环境 | 结果 |
|---|---|
| macOS（`cargo test --workspace`） | 588 通过，0 失败，1 忽略 |
| Linux 容器（`scripts/linux-test.sh`） | 592 通过，0 失败，15 忽略 |
| `cargo fmt --check`、`cargo clippy -D warnings` | 干净 |

### 测试机清单（a–o）

| # | 项目 | 结果 |
|---|---|---|
| a | 自启后的托盘 | ✅ 重启登录后托盘出现，不弹窗口，菜单首项版本与 `cellglide-gui --version` 一致 |
| b | 面板 | ✅ 左键出面板、不会开出第二个。GNOME Wayland 下面板**拿不到焦点**，点桌面别处不会关闭；靠再点托盘、Esc 或 60 秒自动关闭。可接受，记为已知行为 |
| c | 右键菜单 | ✅ 版本、连接 / 断开、打开 CellGlide、退出，顺序正确，连接 / 断开有效 |
| d | 图标状态 | ✅ 连接时为带波纹的图标，断开为普通图标；飞行模式期间出现橙点，恢复后消失 |
| e | Dock 与标题 | ✅ Dock 只有一个 CellGlide 图标；窗口标题「CellGlide · 已连接」；侧栏底部与关于页版本一致 |
| f | 概览与卡片 | ✅ 卡片与 `cellglide status` 一致；切走期间卡片变淡、走势出现缺口 |
| g | 测速 | ✅ 一次完整测速（2026-10-20 17:05Z）：手机 358.4 / 37.9 Mbps、98.9 ms；Wi‑Fi 261.1 / 19.3 Mbps、131.0 ms；Cricket 5G NSA。弹出 1.3 GB 提醒；勾「不再提示」后 `gui.json` 中 `speed_confirmed: true`。连上 M3 那次，整个开发过程共两次完整测速 |
| h | DNS 泄漏 | ✅ 与 `cellglide dns-leak` 一致 |
| i | 连接记录 | ✅ 飞行模式：phoneUnstable 开始 → 7 秒后升级为 phoneNetwork → 结束 → tunnelStopped「released noExit」→ 重连；M3 清单 c 的修复在真机上重验通过 |
| j | 诊断 | ✅ 未预览时「发送」不可用；预览分页；报告 CG-3ZFV4 进入 R2（`platform` 为 linux，检查全部 ok）；保存的文件在下载目录，权限 0600，「打开所在文件夹」有效。用户没填备注，所以报告里没有 contact，符合预期 |
| k | 设置 | ✅ 自动连接、选择手机与 `cellglide settings` 一致；关闭登录启动后重启没有托盘，重新打开后覆盖文件被删除；拔插手机后窗口打开；以 `x` 经两个 SOCKS 端口 `curl` 成功（出口 AT&T IPv6）；检查全绿 |
| l | 睡眠唤醒 | ✅（修复后）见下节。`systemctl suspend` 加 40 秒 RTC 唤醒：日志有 `health judge reset on resume`，唤醒后 26 秒自动重连 |
| m | CLI 遗留 | ✅ 断开后 `status` 打印健康行；`sudo cellglide diagnostics` 打印「不需要 sudo」提示；未连接时 `speedtest` 先打印「没有连接 iPhone，这次只测 Wi‑Fi 或有线。」 |
| n | 关于 | ✅ Telegram 打开 t.me/cellglide；开源许可分页显示 |
| o | 网络不变 | ✅ 与安装前快照相比，只有 M3 记录过的无关差异：重启后 tailscale0 链路序号变化、插手机多出 ipheth 网卡（DOWN、无 DNS）、resolv.conf search 行顺序（Tailscale）。cellglide0 以外没有任何网卡的 DNS 或路由被改。测速前后 `ip route show table all` 与 `resolvectl status` 也一致 |

### KDE Plasma（Lima 虚拟机，不接手机）

Ubuntu 24.04 arm64、Plasma 5.27.11，b68fed4 开发安装，无界面窗口。深色面板修复前后的截图见 `m4-kde/`，其余截图未存档。

- 托盘：Wayland 下图标出现。深色面板上图标几乎看不见：SVG 的 `<style>` 缺 `id="current-color-scheme"`，Plasma 不替换颜色。356e6b0 给四个托盘 SVG 加上该 id 并加测试，修复后在深色面板上变为浅色。
- 面板：X11 下左键打开、点别处关闭、再点打开都正常。Wayland 下无法注入输入，焦点行为未验证。
- 右键菜单四项顺序正确。主窗口七页都能打开，概览显示「等待 USB 连接」，连接记录有「后台服务启动」，中英文都正常。
- X11 下窗口图标是通用图标（开发安装没有装桌面文件），正式包不受影响。

### 睡眠唤醒后 usbmuxd 丢手机（测试 l，2026-10-20）

Ubuntu 26.04（usbmuxd 1.1.1-7、libusb 1.0.29）上，唤醒时内核是把 iPhone 的 USB 设备“重置”，而不是重新枚举。usbmuxd 记下 `Removed device 1`，之后再也不会把它加回来：`ListDevices` 一直为空，而 sysfs、`lsusb` 里 05ac:12a8 和 ipheth 都已回来。CellGlide 到 usbmuxd 的监听连接一直正常，只是收不到 Attached。`systemctl restart usbmuxd` 后约 2 秒手机即回来。这是 usbmuxd 的问题，我们用唤醒钩子绕开：包里带 `/usr/lib/systemd/system-sleep/cellglide`，唤醒（`post`）时用 `systemd-run --no-block` 延迟 3 秒启动 `cellglide-service resume-check`（以 root 运行，服务本身不增加任何权限）。它在 sysfs 里找带 usbmux 接口（ff/fe/02）的苹果设备；找到而 usbmuxd 在 10 秒内一直列不出设备时，执行 `systemctl try-restart usbmuxd.service`；连不上 usbmuxd 时不重启。所有错误只写日志，退出码恒为 0，绝不影响唤醒。只要 usbmuxd 列出任何一台设备就不重启，所以同时接着 iPad 或另一台手机时，丢掉的那台 iPhone 不会被救回，需要重新插拔；这是有意的，免得打断仍在用的设备。临时单元带 `NoNewPrivileges`、`ProtectSystem=strict`、`ProtectHome`、`PrivateTmp`，开发安装（`scripts/install-dev.sh`）也装这个钩子，路径改为 `/usr/local/bin`。

实机复测（2026-10-20 10:28）：唤醒后 usbmuxd 再次丢失手机，`resume-check` 在第 23 秒重启 usbmuxd，第 26 秒 CellGlide 自动重连。审查后 ce3bc65 去掉了固定的单元名并加上 `--collect`，避免连续两次唤醒或上次检查失败时悄悄跳过检查。

### 最终审查后的修正与复测（2026-10-20）

整个分支的最终审查（fd60be0..c910d47）提出两个重要问题与 8 个小问题，修正在 e9098bd..a673579（第 8 个小问题留到 M5）。自动测试：macOS 599 通过 / 1 忽略，Linux 容器 603 通过 / 16 忽略，clippy 干净。测试机装 a673579 的 x86 包后实测：

- 单实例：主窗口关闭、托盘在运行时从应用列表打开两次，始终只有一个窗口和一个托盘图标（✅）。升级前就在运行的旧界面不持有 `com.cellglide.Gui`，升级后第一次打开会多出一个图标，退出旧进程即可；发布说明里要写。
- 服务停止：`systemctl stop cellglide` 期间托盘变为普通图标，右键为「连接」；启动后自动恢复（✅）。
- 带沙箱属性的唤醒钩子：`systemctl suspend` 加 40 秒 RTC 唤醒，日志有 suspend / resume 两条重置；`resume-check` 在唤醒后 17 秒重启 usbmuxd，手机自动重连（✅）。
- 网络快照前后只有 iPhone USB 网卡的链路序号变化（唤醒后重新枚举）。

### 留到 M5

- GNOME Wayland 下面板拿不到焦点，点别处不关闭（已有再点、Esc、60 秒三种关闭方式）。
- KDE Wayland 下面板的位置与焦点未实测。
- 发布前重跑 `scripts/update-licenses.sh`，版本号越过 1.0.0~beta.2。
- R2 里的测试报告 CG-FEX8W、CG-J9U6G、CG-3ZFV4 由用户决定是否删除。

## M5 发布

日期：2026-10-20～21。包从 `linux-1.0-full` @ c713fe3 构建：arm64 在 Mac 的 Docker（colima）里打，x86_64 在测试机的 podman 里打，两边都用 Ubuntu 22.04 构建容器。另外在 ARM VPS（Debian 12 aarch64，docker）上试打一次 arm64，deb 与 Mac 打出的逐字节相同（rpm 只差打包时间）。以后 arm64 改在这台 VPS 上打，打完清理。

### 版本排序

- `dpkg --compare-versions 1.0.0-1 gt 1.0.0~beta.2-1`：成立（debian:12）。
- `rpm.vercmp("1.0.0-1", "1.0.0~beta.2-1")` = 1（fedora:42）。

### 六个发行版（Lima arm64，`scripts/test-install-vm.sh`）

| 发行版 | 全新安装、检查、卸载 | 从 beta.2 升级 |
|---|---|---|
| Ubuntu 22.04 | ✅ | |
| Ubuntu 24.04 | ✅ | ✅ |
| Ubuntu 26.04 | ✅ | |
| Debian 12 | ✅ | |
| Debian 13 | ✅ | |
| Fedora 42 | ✅ | ✅ |

每个发行版：服务以 `cellglide` 用户运行、开机启用；polkit 允许服务调用 systemd-resolved；`ip route flush … realm 198` 可用；`cellglide doctor` 除「没有插着的 iPhone」外全过；卸载后无残留。升级测试另验了检查 f：停用的服务升级后仍停用，启用的服务重装后重新启动。日志在 `build.noindex/logs/m5-vm-*.log`。

### 本地发布预演（`scripts/test-publish-local.sh`，一次性密钥，不上传）

全部通过：错误密钥、缺发布记录、记录不符、缺签名副本都被拒绝；`release-linux.json` 的四个链接在本地服务器上都能取到；Ubuntu 虚拟机经 `setup-apt.sh` 一行命令安装，卸载、重装、清除正常；Fedora 虚拟机先按 URL 装 beta.2，再 `dnf upgrade --refresh` 升到 1.0.0。第一次预演在 Fedora 一步失败，原因是脚本仍按「当前版本比上一版旧」写的升级方向，5c63de5 改成先装旧版再升到新版后通过。

### 测试机实机冒烟（x86_64，接 iPhone）

先拍网络快照、停服务、确认 `cellglide0` 已消失，再 `dpkg -i` 装 1.0.0-1：服务自动重连。用户注销后重新登录：托盘与右键菜单显示 1.0.0；面板卡片有数据；主窗口侧栏底部与关于页为 1.0.0；七页都能打开；DNS 泄漏检测出结论；从应用列表打开两次只有一个窗口和一个托盘图标（✅）。`systemctl suspend` 加 40 秒 RTC 唤醒：有 suspend / resume 两条重置日志，唤醒后 14 秒 `resume-check` 重启 usbmuxd，自动重连（✅）。网络快照前后只有 iPhone USB 网卡链路序号变化与 resolv.conf search 行顺序（Tailscale，M1 起已见），cellglide0 以外没有网卡的 DNS 或路由被改。

### 正式发布（2026-10-21）

- 用户在自己的终端运行 `sign-and-publish.sh`（正式密钥），1.0.0 上传到 pkg.cellglide.com；beta.1、beta.2 仍在源里。四个直链返回 200，apt 索引列出 1.0.0-1，测试机 `apt-cache policy` 候选为线上 1.0.0-1。
- 线上冒烟：临时 Lima Ubuntu 24.04 经 `setup-apt.sh` 装到 1.0.0-1，Fedora 42 按 README 加源后 `dnf install` 装到 1.0.0-1，服务都在运行。
- 官网从 CellProxy `linux-download` 部署（Worker 版本 b41fb520-425f-4a29-816b-0c84975e1b79）：三种语言的下载页有 1.0.0 的 Linux 下载区；Mac 的 `appcast.xml` 部署前后逐字节相同。
