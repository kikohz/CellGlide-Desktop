# Windows 1.0 W1 验证记录

机器：Windows 11 专业版 23H2 (22631) x64，管理员账户（提权不弹窗）。日期：2026-10-30。

## 开发用安装脚本（Task 14 Step 1）

脚本：`scripts\install-dev.ps1`，需要管理员 PowerShell：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 [-Release]
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 -Uninstall [-Purge]
```

### 安装做了什么

1. `cargo build [--release] -p cg-service -p cg-cli`，`fetch-wintun.ps1 -Dest target\<profile>`。
2. 停止已有的 CellGlide 服务，等 `cellglide0` 消失（最多 10 秒，超时则失败）。
3. 复制 `cellglide-service.exe`、`cellglide.exe`、`wintun.dll`、`wintun-LICENSE.txt` 到 `C:\Program Files\CellGlide\`。
4. 新建服务（`New-Service`，路径带引号，自动启动，LocalSystem）；服务已存在时用 `Win32_Service.Change` 更新路径（不用 `sc.exe config`：Windows PowerShell 5.1 传参时会丢掉 `binPath=` 里的引号，留下带空格的无引号路径）。
5. `sc.exe failure CellGlide reset= 86400 actions= restart/2000/restart/5000/restart/30000`；`sc.exe sidtype CellGlide unrestricted`。
6. `sc.exe privs CellGlide SeChangeNotifyPrivilege/SeImpersonatePrivilege/SeLoadDriverPrivilege/SeCreateGlobalPrivilege/SeAuditPrivilege`（Step 3 实测的最小集合，见文末）。
7. 建 `C:\ProgramData\CellGlide\{health,logs}`：先把所有者设为 Administrators 并 `/reset`（防止普通用户预先建好的目录残留权限），根目录 `/inheritance:r`，SYSTEM 和 Administrators 完全控制，`NT SERVICE\CellGlide` 修改；`health\` 另给 Users 读取；`logs\` 只有继承来的三项（Users 无任何权限）。
8. `Start-Service CellGlide`，等到 Running。

### 卸载做了什么（尽力而为，可重复运行）

停服务 → 等 `cellglide0` 消失（≤10 秒）→ `sc.exe delete CellGlide` → 删 `C:\Program Files\CellGlide\` → 删 `Tcpip` / `Tcpip6` 下 `Interfaces\{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}` → 如有 non-present 的 `SWD\Wintun\{E490EEF2-...}` 设备（崩溃留下的 CM_PROB_PHANTOM），`pnputil /remove-device` 删除 → 保留 `C:\ProgramData\CellGlide\`（`-Purge` 时删除）。某一步失败只警告，最后退出码 1。

### 结果

安装后：

- `Get-Service CellGlide`：Running，Automatic；`sc.exe qc`：`BINARY_PATH_NAME : "C:\Program Files\CellGlide\cellglide-service.exe"`，`SERVICE_START_NAME : LocalSystem`；`qfailure` 三次重启 2/5/30 秒，重置 86400 秒；`qsidtype`：UNRESTRICTED；`qprivs`：空（默认）。
- `cellglide.exe status`：`No iPhone connected`（没插手机，符合预期），退出码 0。
- `cellglide.exe doctor`：6 项检查，5 项通过，"No iPhone is plugged in" 未通过（没插手机），退出码 1。
- 服务日志：`C:\ProgramData\CellGlide\logs\service.log.2026-10-30`，内容 `cellglide-service ready on \\.\pipe\cellglide-control`。

ACL（icacls）：

```
C:\ProgramData\CellGlide NT SERVICE\CellGlide:(OI)(CI)(M)
                         BUILTIN\Administrators:(OI)(CI)(F)
                         NT AUTHORITY\SYSTEM:(OI)(CI)(F)
C:\ProgramData\CellGlide\health BUILTIN\Users:(OI)(CI)(RX)
                                NT SERVICE\CellGlide:(I)(OI)(CI)(M)
                                BUILTIN\Administrators:(I)(OI)(CI)(F)
                                NT AUTHORITY\SYSTEM:(I)(OI)(CI)(F)
C:\ProgramData\CellGlide\logs NT SERVICE\CellGlide:(I)(OI)(CI)(M)
                              BUILTIN\Administrators:(I)(OI)(CI)(F)
                              NT AUTHORITY\SYSTEM:(I)(OI)(CI)(F)
```

轮次与快照（`scripts\network-snapshot.ps1`，`cellglide0` 及其 GUID 不参与比较）：

| 轮次 | 操作 | 结果 |
| --- | --- | --- |
| A1 | 安装（debug）→ 卸载 | 两次都成功；与基线 A 比较有差异，见下 |
| A2 | 安装 → 再次安装（更新路径）→ 人为制造 phantom 设备 → 卸载 | 成功；phantom 设备被删除 |
| A3 | 安装（`-Release`）→ `-Uninstall -Purge` → 干净机器上再 `-Uninstall` | 都成功（退出码 0），数据目录已删除 |
| B | 新基线 B → 安装 → 卸载 → 安装 → `-Uninstall -Purge` | 安装后、第一次卸载后、最后卸载后与基线 B 比较都是 `identical` |

A 轮的差异都来自局域网，与 CellGlide 无关（服务没有创建网卡，没插手机）：

- Wi-Fi 上新增 `fd55:ab04:2c04:48cc::/64` 及两个地址：来源 RouterAdvertisement，生命期约 29 分钟，是局域网里的路由通告。
- Wi-Fi 的 `Tcpip6\...\Dhcpv6InterfaceOptions` 变化：把 4 字节时间戳屏蔽后内容完全相同；记录里 T1 = 0x546 = 1350 秒，上次续租 10:14:00Z + 1350 秒 = 10:36:30Z，正是这次的新时间戳，即 DHCPv6 按 T1 定时续租。快照脚本的易变值正则没有覆盖这个值。

`scripts\tests\test-network-snapshot.ps1`：`all passed`。

最终状态：服务、程序目录、数据目录、`cellglide0`、phantom 设备、Tcpip 接口键都不存在。

其他观察：服务启动时 `cleanup_leftovers` 清理我们 GUID 的 DNS，会重新建出 `Tcpip\...\Interfaces\{E490EEF2-...}` 键（只属于 cellglide0，快照不比较），卸载时删除。

## 实机端到端（Task 14 Step 2）

手机：iPhone 16e（iPhone17,5），iOS 26.6，App Store 版 CellGlide 2.4.0，Cricket（AT&T 网络）5G NSA，USB 接 Apple 设备应用（AMDS，127.0.0.1:27015）。电脑：Wi-Fi（出口 `172.58.116.225`）、Tailscale（Up）、蓝牙 PAN（未连接）。安装：`scripts\install-dev.ps1 -Release`。

基线快照在安装之前拍（`target\t14b\snap-0-baseline.json`，08:40 本地）；安装后、手机还没共享时与基线比较 `identical`。手机 App 未共享时服务按退避重试，日志为 `connect failed: phone service not ready`（usbmuxd 拒绝连 App 的 8088 端口），用户在手机上开始共享后服务自动连上。

快照差异说明：08:51 前后 Wi-Fi 的 DHCPv6 按 T1 续租，`Tcpip6\{f870e3e0-...}\Dhcpv6InterfaceOptions` 里 4 字节时间戳从 `7c60e56a` 变成 `c265e56a`（相差 0x546 = 1350 秒，正好是 T1），把时间戳屏蔽后两个值完全相同（与 Step 1 A 轮的现象一样，快照脚本的易变值正则没覆盖这个值）。所以续租之后的快照既和基线比，也和续租后第一张断开状态快照（`snap-4-afterkill.json`）比；除这一个值外都 `identical`。

### 1. 连接、出口、DNS —— 通过

- `cellglide.exe status`：`Connected. This computer is using the phone's network (CellGlide 2.4.0 on the phone)` / `Cricket · 5G NSA · iPhone 16e · 79% (charging)` / `Connection OK`。
- `curl.exe -s https://www.cloudflare.com/cdn-cgi/trace`：`ip=2600:387:15:1339::4`（IPv6）；`curl.exe -4`：`ip=166.194.147.76`。两个都是 AT&T Mobility 的地址（Cricket 走 AT&T），不是 Wi-Fi 的 `172.58.116.225`。
- `Resolve-DnsName example.com`：成功（A `104.20.23.154`、`172.66.147.243`，AAAA 两条）。
- `Get-DnsClientServerAddress -InterfaceAlias cellglide0`：IPv4 `{198.18.0.1}`，IPv6 `{}`。
- cellglide0：地址 `198.18.0.1/32`、`fd6c:6779:6465::1/128`，MTU 1500，路由 `0.0.0.0/1`、`128.0.0.0/1`、`::/1`、`8000::/1`（跃点 0）。
- 严格 DNS：服务日志 `INFO cg_platform::windows::backend: strict DNS on: port 53 only through cellglide0`，之后每次连接都有这一行。

### 2. 物理网卡上的 DNS 包 —— 通过（0 个）

`pktmon filter add -p 53`，`pktmon start --capture --comp 12 13 1`（蓝牙 PAN、Wi-Fi AX201，另加 Tailscale），60 秒（08:49:34–08:50:36）里循环访问 20 个网站：55 次 HTTPS 请求（49 次 2xx/3xx），每 20 次清一次 DNS 缓存，每次另做一次 `Resolve-DnsName -DnsOnly`。停止前 `pktmon counters`：`All counters are zero`，即 Wi-Fi、蓝牙、Tailscale 上 0 个 53 端口的包。

对照：随后把过滤器换成 443/41641 端口抓 5 秒，Wi-Fi 上计到 Rx 11 / Tx 10 个包，说明抓包本身是有效的。

### 3. 断开 —— 通过

`cellglide.exe disconnect` → `Disconnected. This computer is back on its own network`，0.83 秒后 cellglide0 消失；`status`：`iPhone connected. Run cellglide connect to use the phone's network`。快照（`snap-3-disconnected.json`）与基线 `identical`。

### 4. 服务被强杀 —— 通过

连接后 `taskkill /F /IM cellglide-service.exe`（PID 12480），每 100 ms 轮询：

| 事件 | 距 taskkill |
| --- | --- |
| cellglide0 消失 | 0.08 秒 |
| 四条 /1 路由消失 | 0.08 秒 |
| SCM 拉起新进程（PID 7156） | 2.06 秒 |
| 自动连接，cellglide0 重新 Up | 2.98 秒 |

系统日志 7031：`The CellGlide service terminated unexpectedly. It has done this 1 time(s). The following corrective action will be taken in 2000 milliseconds: Restart the service.` 重连后出口仍是 `166.194.147.76`，DNS 仍只有 `198.18.0.1`，日志有 `strict DNS on`。没有留下 phantom 设备。断开后快照（`snap-4-afterkill.json`）与基线只差上面说明的 DHCPv6 续租时间戳。

### 5. 拔线 / 插回

**通过，有附注**（2026-10-30，用户在场）。注意：用户执行过 `disconnect` 后，`user_disconnected` 会写进设置，自动连接要等下一次 `connect` 才恢复；测之前先 `cellglide.exe connect` 了一次。

- 拔线：16:15:51Z 健康检查报 `NoUsbService`，会话释放，cellglide0 消失，`status` 显示 `No iPhone connected`。
- 第一次插回：PnP 里 iPhone 正常（`VID_05AC&PID_12A8`），但 Apple 设备的 usbmuxd `ListDevices` 返回空列表（原始查询和 `phone_probe` 都是），手机上显示 USB 只在充电。CellGlide 按退避重试（日志写 `usbmuxd not reachable`，这个措辞对「端口在、但没有设备」的情况不准，待改）。系统日志显示 16:15:01Z–16:22:19Z 机器处于现代待机，和这次拔插重叠，很可能是原因。
- 第二次插回：usbmuxd 列出手机，16:24:54Z 自动连接，严格 DNS 重新开启。

### 6. 睡眠 / 唤醒

**部分通过，发现问题**（2026-10-30，用户在场）。

- 这台机器只支持现代待机（`powercfg /a`：Standby (S0 Low Power Idle) Network Connected）。brief 里的 `rundll32.exe powrprof.dll,SetSuspendState 0,1,0` 被系统拒绝（Kernel-Power 187），机器没有睡下去。
- 改为用户短按电源键：16:28:11Z 进入、16:28:23Z 退出现代待机。连接保持不断，唤醒后 `status` 仍为已连接；日志只有一条「battery details unavailable」。
- **问题**：两次现代待机（上面这次和第 5 项期间 16:15–16:22 的那次）服务日志里都没有 Suspend / Resume 事件，也没有墙钟跳变记录。现在的电源处理只覆盖 S3 睡眠（`PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC`）。现代待机要另外处理（例如注册 `GUID_CONSOLE_DISPLAY_STATE` 的 `PBT_POWERSETTINGCHANGE`，退出待机时按唤醒处理、立即重新探测手机），列入后续任务。传统 S3 睡眠这台机器测不了。

### 7. 诊断包 —— 通过

`cellglide.exe diagnostics --save target\diag.json` → `Saved to target\diag.json`，exit 0，3193 字节。不区分大小写搜索：Windows 用户名（`$env:USERNAME`）0 处，计算机名（`$env:COMPUTERNAME`）0 处。内容：`app 1.0.0`、`platform windows`、`os "Windows 11 Pro 23H2 (build 22631)"`、`environment`（检查项全部 ok、State `connected (phone 2.4.0, dns phone)`、Phone `AT&T · 5G NSA`）、`sessions` 1 条、`timeline` 10 条、`log` 14 行服务日志（`cellglide-service ready ...`、`strict DNS on ...` 等）。

### 8. 测速（只跑了一次完整测速）—— 下行有数，上行 0.0

`speed/transport.rs` 原来只在 Linux 上能绑网卡，第一次 `speedtest -y` 0.7 秒就失败了：`Could not find this network interface. Check Wi‑Fi and try again`（没有发出任何测速流量）。补上 Windows 的 `IP_UNICAST_IF` / `IPV6_UNICAST_IF` 绑定后（提交 `f081474`），只跑了**一次**完整测速（08:56:26 开始，20.9 秒，exit 0）：

```
iPhone
  Latency 139 ms · Download 195.4 Mbps · Upload 0.0 Mbps
Speed test finished
```

- 测速期间每秒采样 cellglide-service 的 CPU（`TotalProcessorTime`，共 25 个样本）：峰值约 0.39% 总 CPU（8 个逻辑处理器，约合 3.1% 单核），工作集 32–39 MB。服务进程从 08:56:09 启动到测完一共只用了 0.36 秒 CPU，这期间 cellglide0 收 227 MB、发 56 MB。另外用 curl 下载 20 MB 做了对照：服务 CPU 只增加 0.06 秒，cellglide0 收到 20.9 MB。
- **上行 0.0 Mbps**：上传期间系统往 cellglide0 写了约 50 MB，但测速结束后手机端 `↑` 还以约 460–590 KB/s 持续发了约 3.5 分钟，`Network quality` 降到 Poor（857–1267 ms），09:00:15 才降到 0。说明大量上传数据积压在电脑之外（netstack 每条连接只有 256 KB 缓冲，积压应该在 usbmux 或手机那一侧），而测速引擎是按套接字收下的字节算上行，预热 2 秒之后几乎没有新字节被收下。积压排空后，用 curl 往 `speed.cloudflare.com/__up` 传了 5 MB：12.0 秒，约 3.3 Mbps。这条线路的上行实际只有几 Mbps；测速数字为 0 这件事留给 W2 查，**没有**重跑测速。
- 本次会话手机一共计了约 340 MB。

## 最小特权集合（Task 14 Step 3）—— 这组特权可用，已写进脚本

`sc.exe privs CellGlide SeChangeNotifyPrivilege/SeImpersonatePrivilege/SeLoadDriverPrivilege/SeCreateGlobalPrivilege/SeAuditPrivilege`，`Restart-Service`。读取服务进程令牌（`GetTokenInformation(TokenPrivileges)`），里面正好是这 5 项（SeLoadDriverPrivilege 存在但未启用，其余 4 项已启用）。

- 第 1 项：重启 2.3 秒后自动连上；出口 `166.194.147.76` / `2600:387:15:1339::4`；`example.com` 能解析；cellglide0 DNS 只有 `198.18.0.1`；四条 /1 路由都在；日志有 `strict DNS on`（WFP 动态会话正常）。
- 第 3 项：`disconnect` 后 1.26 秒网卡消失；快照与续租后的 `snap-4` 比 `identical`。
- 另外又做了一次第 4 项：`taskkill` 后 0.10 秒网卡消失；这是 24 小时内第二次失败，SCM 按 5 秒延迟在 5.13 秒拉起服务，5.93 秒重新 Up；`doctor` 7 项全部通过（exit 0）；断开后快照与 `snap-4` 比 `identical`。

`install-dev.ps1` 现在会设置这组特权。用改过的脚本重装后：`sc qprivs` 是这 5 项，`qc` 路径带引号，自动启动，LocalSystem，`qfailure` 为 2/5/30 秒、重置 86400 秒，`qsidtype` 为 UNRESTRICTED。

最终状态（09:06）：服务已安装、Running，隧道断开（`user_disconnected`），没有 cellglide0，没有 phantom 设备；快照与基线只差 DHCPv6 续租时间戳，与 `snap-4` 比 `identical`。

## 上行 0.0 调查（W2 Task 4，2026-10-30）

工具：`cargo run -p cg-tools --example speed_probe -- --route phone --down-mb 5 --up-mb 5 --seconds 5`（小数据量探针），同时每秒 `cellglide.exe status` 看手机卡片的 `↑`。这天手机（Cricket 5G NSA）上行比 W1 快得多：curl 往 `__up` 传 5 MB 用 2.02 秒，**19.8 Mbps**（W1 是 3.3 Mbps）。服务里临时加了计数（查完已删）：② cg-netstack 每条会话写给 SOCKS 的字节（`tcp.rs` 的 `uplink`），③ 转发器写进 usbmux 连接的字节（`copy_bidirectional` 临时换成计数拷贝），另外记下手机那一侧何时回 EOF。

- **复现（修改前）**：6 条流各 833 KB，① 引擎在 0.5 秒的第一个 tick 就算完 5 MB（cap reached），报上行 **79.3 Mbps**；② 每条 835,277 字节在 85–91 ms 内全部交给 SOCKS；③ 转发器在 152–157 ms 内全部写进 usbmux；**手机在 0.3–0.4 秒后就回了 EOF**（说明我们的数据和 FIN 都已到手机），可手机 `↑` 之后还以 2.15–2.50 MB/s 持续约 3 秒。单流 10 MB、1 秒（时间到才结束）：② 0.44 秒写完 10 MB，③ 0.15 秒 4.3 MB、0.49 秒 7.0 MB、1.99 秒 10 MB（这里有约 3 MB 暂时在 netstack→转发器的本机回环套接字里），手机 2.7 秒时回 EOF，`↑` 又持续约 2 秒（约 5 MB 在手机里）；引擎报 79 Mbps，手机实际约 20 Mbps。W1 那次是 6 × 50 MB 的大请求、10 秒，积压约 50 MB，在 3.3 Mbps 的上行上排了几分钟，而引擎 2 秒预热之后几乎没有新字节被套接字收下，所以是 0.0。
- **积压在哪**：在我们进程之外，主要在手机里。②≈③（同一批字节，几十毫秒之差），③ 远大于手机实际 `↑`，但手机已经读完了数据和 FIN（回了 EOF）还在往蜂窝发，说明是手机自己的发送缓冲（iOS 那一侧）在排队；收紧我们这边的缓冲（`SOCKET_BUFFER`、转发器的 `SO_SNDBUF`）挡不住手机吃下几 MB 到几十 MB。
- **测速结束后 cg-netstack 对 SOCKS 一侧**：修改前是**发 FIN**（客户端 RST 后会话被清理，流任务按 `End::Gone` 正常关闭），转发器把 FIN 传给手机，手机把手里的数据发完。W1 终审 T9 遗留的 TIME-WAIT 情形（同一四元组复用时带着未读数据的会话被清掉）走的也是这条路，误报成正常结束。
- **修法：做法 B**（理由：积压在手机，只能改计量，不能靠收紧我们这边的缓冲）。引擎按服务器确认的字节计上行：上传请求一律 `UPLOAD_REQUEST`（1 MB，`Params.request_size` 只管下载），一个请求收到 `__up` 的 200 回复才把它的字节计入，同一连接上一个接一个发；上传套接字设 `SO_LINGER 0`，测完关闭时发 RST，缓冲里剩下的不再发出。cg-netstack 在客户端没发 FIN 就消失时（RST、会话被清理）向 SOCKS **中止**（零 linger），转发器随之丢掉 usbmux 连接；只有服务停止时才正常关闭。所有平台同一算法，Linux 同样生效。
- **修改后实机（小数据量）**：同一命令跑两次，上行 **19.8 Mbps**、**66.2 Mbps**；curl 对照 **19.8 Mbps**。第二次偏高是小上限的量化问题：每条流只有一个 833 KB 的请求，6 个确认挤在 2.0–2.5 秒之间，预热点（2.0 秒）之后 0.5 秒内算进了 4.2 MB；第一次的确认都在 2.0 秒之前到，算出的是从头平均。另跑一次每条流 5 个请求（`--streams 2 --up-mb 10 --down-mb 2 --seconds 5`）：**23.8 Mbps**，手机 `↑` 当时 1.9–3.5 MB/s。测完后手机 `↑` 1–2 秒内回到 0（3 次都是）。时间到才结束的单流 1 秒（`--up-mb 10 --seconds 1 --streams 1`）：引擎只算已确认的 1 MB（7.9 Mbps），关闭时转发器那条连接以错误结束（SOCKS 一侧被中止），手机 `↑` 约 3 秒内回到 0，剩下的不到 1 MB 是手机已经收下的。下行 26.4 Mbps（两次）、15.8 Mbps，和 W1 同量级（W1 是完整测速 195 Mbps，这里每条流不到 1 MB，主要量的是慢启动）。
- **手机流量（第一轮）**：**63.0 MB**（`This session` 从 26.2 MB 到断开时的 89.2 MB）。其中修改前 2 次探针加后台流量 22.1 MB；修改后 4 次探针加 curl 5 MB，共 40.5 MB；最后 0.4 MB 是重装、断开期间的后台流量。
- 网络快照：当天新拍基线，装服务、连接、测量、断开之后再拍，`-Compare` 结果 `identical`。
- **评审后修正（fix round 1）**：上面第二次的 66.2 Mbps 和慢上行的问题都修了，只改上传方向：
  - 预热后确认的请求少于 2 × 流数，或者阶段因为达到上限而结束时，上行按从阶段开始算的平均：`mbps(moved, elapsed)`。
  - 时间到了还没有足够的确认时，不再发新请求，给已经在路上的请求一个宽限期等确认，最多再等一个 `duration`（默认 10 秒）。这时速率 = 已确认字节 ÷ 最后一次确认的时间。宽限期内一个确认都没有，上行就不给数字（`None`），并加一条说明（`speed.upload_too_slow`），不再显示 0.0。
  - 宽限期里进度事件照常发出，`elapsed` 停在 `duration`，不会倒计时到负数。
  - 预热后确认够多时，仍用 Mac 的预热后速率。
  - 在 W1 那样 3.3 Mbps 的上行上，第一批确认约 14.5 秒才到，现在落在宽限期内，算出约 3.3 Mbps。
- **修正后实机复测**（服务已重装；当天新拍快照，前后 `identical`）：同一命令跑两次，上行 **19.7 Mbps**、**26.4 Mbps**；curl 对照 **24.4 Mbps**（5 MB，1.64 秒）。两次都在 ±50% 内（−19%、+8%）。下行 26.5 Mbps 两次。测完后手机 `↑` 约 1 秒内回到 0。这一轮用了 **25.3 MB**（89.2 → 114.5 MB）。本任务两轮合计 **88.3 MB**。

## 严格 DNS 开关（W2 Task 7，2026-10-30）

设置里多了一个开关 `strict_dns`（默认开；旧的 settings.json 读出来也是开）。命令行：`cellglide settings --strict-dns on|off`，`cellglide settings` 在 Windows 上多打一行「严格 DNS：开/关」。连接中切换立即生效，不重连：关 = 关闭 WFP 动态会话（我们的子层和全部过滤器随之删除）；开 = 按连接时同样的条件（cellglide0 的 DNS 设置成功）重新建会话，虚拟网卡的 LUID 放行重新枚举。关闭时 `status` 显示「严格 DNS：已在设置中关闭」，诊断写 `StrictDns: disabled`，不记时间线事件；打开失败照旧记 `strictDnsUnavailable`。设置为关时连接，不建 WFP 会话。

实机（release 服务重装，iPhone 16e / Cricket 5G NSA，Wi-Fi DNS `10.10.10.1`）：

| 步骤 | `status` | 诊断 `StrictDns` | `netsh wfp show filters`/`show state`（只读） | pktmon，Wi-Fi 网卡，端口 53，20 秒 | `Resolve-DnsName example.com -Server 10.10.10.1` |
| --- | --- | --- | --- | --- | --- |
| 连接后（默认开） | 严格 DNS：已开启 | `on` | 子层在，18 个过滤器 | 0 包（3 轮查询，全部超时） | 超时 |
| `--strict-dns off` | 严格 DNS：已在设置中关闭 | `disabled` | 子层不在，0 个过滤器 | **266 包**（Rx 130 / Tx 136；32 轮，32 次直接查询都有应答） | 有应答 |
| `--strict-dns on` | 严格 DNS：已开启 | `on` | 子层在，18 个过滤器，与第一次逐条相同 | **0 包**（3 轮，全部超时） | 超时 |
| 断开 | 已断开 | — | 子层不在，0 个过滤器 | — | — |

- 18 个过滤器 = 两层（`ALE_AUTH_CONNECT_V4/V6`）各：1 个按接口类型 6/71/243/244 拦 53 端口、6 个虚拟网卡 LUID 放行、1 个回环放行、1 个 cellglide0 LUID 放行。重新打开后按层、动作、权重、条件比较，与连接时的那份完全一样（过滤器 ID 不同）。
- 网络快照：重装服务前新拍基线，连接、两次切换、断开之后再拍，`-Compare` 结果 `identical`。特权测试（含新加的 `strict_dns_toggles_on_a_running_tunnel`、`strict_dns_off_in_the_plan_starts_without_rules`）前后也是 `identical`。
- 结束时设置恢复默认（严格 DNS 开），隧道已断开；pktmon 过滤器列表清空（开始前也是空的）。
- 不测速。手机流量约 0.3 MB（`This session` 242 KB，加上断开前的少量后台流量）。

## W2 cellglide_native.dll（W2 Task 13，2026-10-30）

工具：`windows/CellGlide.Probe`（控制台小程序，经 CellGlide.Core 的 `NativeCore` + `EventHub` + `InlineDispatcher` 调 `cellglide_native.dll`），在 `windows\` 里运行：`dotnet run --project CellGlide.Probe -- status|speed-small|leak`。每行是「时间 + 事件名 + DLL 给的文字」，程序自己只写用法说明。环境：iPhone 16e / Cricket 5G NSA，Wi‑Fi 对比网卡，debug 服务用 `scripts\install-dev.ps1` 重装为当前 HEAD。

| 项目 | 结果 |
| --- | --- |
| 1. `status --seconds 60`，期间 `Restart-Service CellGlide`（23:10:34.7 发出，0.6 秒返回） | **通过**。23:10:34.847 `service_gone`「The CellGlide service stopped」；23:10:36.857 恢复 `state`（Waiting for USB → Checking the phone's exit），同时 `phone_plugged 00008140-…`；23:10:37.398 「Connected, this computer is using the phone's network」+「Strict DNS: on (DNS queries can only go through the phone)」，手机卡片「Cricket · 5G NSA」。服务停到恢复连接约 2.6 秒，C# 没有重新订阅。退出码 0。 |
| 2. `speed-small`（caps 下行 10 MB、上行 5 MB、5 秒，include_wifi，confirmed） | **通过**。iPhone 腿 Done：延迟 119 ms、下行 **39.7 Mbps**、上行 **1.6 Mbps**（经过宽限期「Waiting for the server to confirm the upload…」后确认，不是 0.0）；Wi‑Fi 腿 Done：79 ms、下行 **78.6 Mbps**、上行 **39.5 Mbps**。verdict「Wi‑Fi download is 2.0x faster than the iPhone.」，历史最新一条 `2026-10-30 23:11 | ↓ 39.7 ↑ 1.6 Mbps · 119 ms | ↓ 78.6 ↑ 39.5 Mbps · 79 ms | Cricket · 5G NSA`。各腿带 cap reached 的说明。退出码 0。手机流量：`This session` 257 KB → 12.3 MB，约 **12.0 MB**。 |
| 3. `leak` | **通过**。leakcheck 服务端接受 Windows 客户端（未出现 `leak.unavailable`）。结论「No leak」：全部查询经 Cloudflare 加密 DNS（1 组 16 个地址），出口「AT&T Enterprises, LLC · Arizona, US」。退出码 0，流量不到 0.1 MB。 |
| 4. `cellglide.exe disconnect` 后拍快照 | **`identical`**（基线在重装服务之前新拍：`target\t13\snap-before.json`，之后 `snap-after.json`）。 |
| 5. Release：`cargo build -p cg-ffi --release`；`dotnet run -c Release -p:CellGlideNativeDir=..\..\target\release\ --project CellGlide.Probe -- status --seconds 10` | **通过**。打印「Disconnected」状态，退出码 0；输出目录里的 DLL 与 `target\release\cellglide_native.dll` 哈希相同。当时相对路径按各项目目录解析，所以写成 `..\..\target\…`；fix round 1 之后相对路径一律从 `windows` 目录算起，写 `..\target\release\` 即可（Debug、Release 下都核对过 DLL 哈希）。 |
| ARM64（只编不跑） | `cargo build -p cg-ffi --target aarch64-pc-windows-msvc` 成功；`dotnet build CellGlide.Core -c Debug -r win-arm64` 成功、0 警告；`dotnet publish CellGlide.Probe -c Release -r win-arm64 --self-contained false` 成功，`CellGlide.Probe.exe` 和 `cellglide_native.dll` 都是 ARM64 PE。锁文件不变（`Directory.Build.props` 的 `RuntimeIdentifiers` 是 `win-x64;win-arm64`，两个 RID 都记在锁文件里）。 |

- 观察：小测速时 iPhone 腿的阶段文字刚进上行时有一帧「Testing Upload… 2 / 10 s」再回到「1 / 10 s」，以及 caps 下仍写「/ 10 s」，都是 Task 10 已记下的阶段文字问题，不影响结果。
- 结束时：隧道已断开，服务 Running。
- **手机流量**：本任务约 **12.6 MB**（重启前的会话约 0.2 MB，重启后的会话到断开 12.4 MB）。**W2 累计约 111 MB**（预算 300 MB）。W2 没有跑过完整测速。
- **Ctrl+C（fix round 1）**：Probe 处理 `Console.CancelKeyPress`（`e.Cancel = true`，退回 `ProcessExit` 兜底），信号一到：正在跑的测试先发 `SpeedCancel`/`LeakCancel`，所有等待都结束，`using` 照常释放，所以总会调到 `cg_shutdown`；退出码 130。实机（Probe 在单独的控制台里运行，另一个进程 `AttachConsole` + `GenerateConsoleCtrlEvent` 发 Ctrl+C）：`status` 263 ms 内退出 130；`leak` 在测试进行中（开始约 0.6 秒后）退出 130；`speed-small` 在 iPhone 腿「Checking HTTPS latency…」时收到 Ctrl+C，DLL 随即发出两条腿「Speed test cancelled」，253 ms 内退出 130，手机 `↑↓` 回到 0。这一轮快照前后 `identical`（`snap-f1-before/after.json`），隧道已断开，手机流量约 **0.1 MB**（`This session` 12.4 → 12.5 MB）。W2 累计仍约 111 MB。

## W3 界面

### 工具链与 App 骨架（W3 Task 1，2026-10-31）

- **没有装任何东西**。`dotnet build CellGlide.sln -c Debug` 只靠 NuGet 包（Microsoft.WindowsAppSDK 2.6.4、H.NotifyIcon.WinUI 2.4.1）就编过了，0 警告，不需要 VS 的 `Microsoft.VisualStudio.ComponentGroup.WindowsAppSDK.Cs` 组件。
- **Native AOT 链接要 `vswhere.exe` 在 PATH 上**。ILCompiler 调 `vcvarsall.bat`，后者按 PATH 找 `vswhere.exe`，找不到时那句报错被拼进了 link.exe 的路径，链接失败。现在 `scripts/dev-env.ps1` 会把 `%ProgramFiles(x86)%\Microsoft Visual Studio\Installer` 加进 PATH。
- **代理的 shell 在会话 0**（没有交互桌面），WinUI 在那里起不来，`Microsoft.UI.Xaml.dll` 以 0xC000027B（E_ACCESSDENIED）崩溃。界面实测改用临时计划任务（`schtasks /it /ru iloss`），在会话 1 里启动、读标题、关窗口，查完就删掉任务。
- Debug 运行：窗口标题 `CellGlide`（DLL 的 `gui.title`）；`CloseMainWindow` 后约 0.3 秒退出，退出码 0。
- **AOT 还是 R2R：用 Native AOT**。`dotnet publish CellGlide.App -c Release -r win-x64 -p:CellGlideNativeDir=..\target\release\`：0 警告（包括 H.NotifyIcon 和 System.Drawing.Icon 被实际用到时），发布出来的 `CellGlide.exe` 标题正确、关窗口约 0.3 秒退出，退出码 0。没有挡路的问题，所以不退回 ReadyToRun。
- **AOT 下的托盘（临时代码，查完已删）**：用 H.NotifyIcon `TaskbarIcon` 加一个临时 .ico，`ForceCreate` 后 `IsCreated=True`，`Shell_NotifyIconGetRect` 返回 S_OK 和任务栏上的位置（物理像素 1573,1020–1613,1080）。按 shell 点击图标时发的回调消息（NOTIFYICON_VERSION_4 的 WM_LBUTTONDOWN/UP、WM_RBUTTONDOWN/UP），`LeftClickCommand` 和 `RightClickCommand` 都被调到了。没能用眼睛确认：会话 1 截屏是全黑的（屏幕锁定或显示器关着），也没有用真鼠标点。
- **去掉 AI/ML 组件**：Microsoft.WindowsAppSDK 元包（H.NotifyIcon.WinUI 也依赖它）会带进 `.AI`、`.ML`、`.Search` 和 `Microsoft.Windows.AI.MachineLearning`，发布目录里因此多了 onnxruntime.dll（22 MB）、DirectML.dll（17.8 MB）、语义搜索的 DLL 等。App 工程直接引用这四个包，并设 `ExcludeAssets="all"`，它们的文件就不再进构建和发布。WinUI 窗口和托盘照常工作（上面两条就是在这个配置下测的）。发布目录从 158.0 MB / 294 个文件降到 102.7 MB / 242 个。
- **发布大小**：win-x64 共 102.7 MB、242 个文件，其中 pdb 27 MB，不含 pdb 75.6 MB；win-arm64 共 109.4 MB，不含 pdb 82.6 MB。
- **ARM64 只编不跑**：`cargo build -p cg-ffi --release --target aarch64-pc-windows-msvc --locked` 后 `dotnet publish CellGlide.App -c Release -r win-arm64 -p:CellGlideNativeDir=..\target\aarch64-pc-windows-msvc\release\` 成功。`CellGlide.exe` 和 `cellglide_native.dll` 的 PE Machine 都是 0xAA64，锁文件没有变化。
- **NuGet 许可**：`scripts/check-nuget-licenses.ps1` 生成 `packaging/licenses/nuget.txt`，`-Check` 已接进 `check-windows-core.ps1`。把 nuget.txt 里一个版本号改掉后，`-Check` 退出 1；删掉 `$Reviewed` 的一项后，脚本报「not in the allowed list and not reviewed」并退出 1。
  - **Windows App SDK 不是宽松许可**（控制者裁定接受）：Microsoft.WindowsAppSDK 元包和各组件包都是 Microsoft Software License Terms，Microsoft.Windows.AI.MachineLearning 是 Windows ML Runtime 的同类条款。条款 §3.a.i 允许把 NuGet 包放到应用旁边的文件随应用分发，自包含部署也算。它们记在 `$Reviewed` 里。WebView2 的许可文件是 BSD-3-Clause 文本，也记在 `$Reviewed` 里。Windows SDK BuildTools / BuildTools.MSIX 标为「build only, not shipped」。
  - **W4 必须做**：zip 和安装包要带上 Windows App SDK 的许可文本和最终用户条款提示（条款 §3.b.ii 要求分发者和最终用户同意至少同等保护微软的条款），`THIRD-PARTY-LICENSES` 也要列上它。
- `scripts\check-windows-core.ps1` 与 `-Publish` 都退出 0：115 个测试通过；`-Publish` 的发布目录里有 `CellGlide.exe` 和 `cellglide_native.dll`。

## 已知问题（W1 实测发现，未修）

- **严格 DNS 下，查不到的名字要等约 12 秒**（1.0 接受，controller 裁定）。连接时查不存在的名字约 12 秒才返回 NXDOMAIN（Task 11c 实测 12.02–12.08 秒；W1 当时用 `Resolve-DnsName nx<随机>abc.com -DnsOnly` 测得 11.2–11.4 秒），断开后只要 0.02–0.39 秒；直接问 `198.18.0.1` 只要 0.15–0.8 秒。原因是 Windows DNS 客户端收到 cellglide0 的否定应答后，还会等物理网卡的 DNS 服务器（例如 Wi-Fi 的路由器），而这些 53 端口的包被 WFP 拦掉，只能等超时；用户态 WFP 没办法让 UDP 发送立即失败。能查到的名字不受影响（0.01–0.4 秒）。Task 11c 起严格 DNS 只拦物理网卡，Tailscale 等其他 VPN 不再被拦，MagicDNS 连接期间可用（W1 当时 `nohost.tail707c29.ts.net` 11.4 秒超时的问题已解决）。
- **严格 DNS 放行的虚拟网卡里，有几类实际经物理网卡承载本机流量，它们的 53 端口会泄漏**：Hyper-V External 虚拟交换机（主机经 `vEthernet (External)` 上网）、Windows「网桥」（MAC Bridge Miniport）、厂商网卡组合（teaming）。它们都是类型 6、`HardwareInterface` 为假，按规则放行（Hyper-V 按 brief 放行）。本机没有这几种环境，未实测。
- **PPPoE 拨号**（Task 11c fix round 1）：PPPoE 链路是 PPP 接口（类型 23），按 LUID 单独拦；内置 VPN（PPTP/L2TP/SSTP/IKEv2）也是 PPP，照常放行。靠 `RasEnumConnections` 的设备类型 `PPPoE` 和条目名（即接口别名）区分；对不上号的 PPP 接口、或 RAS 查询失败时，一律拦（偏向拦截，这时内置 PPP VPN 在连接期间会失去 DNS）。本机没有 PPPoE，未实测，只有单元测试。
- **严格 DNS 的 Task 11c（2026-10-30）复测与处理**（针对第一条的 12 秒和 MagicDNS，与 PPPoE 无关）（Wi-Fi DNS 这时是 `192.168.1.254` / `2600:381:7368:4c73::1`，Tailscale 已登录）：
  - 严格 DNS 改为只拦物理网卡（以太网 6、Wi-Fi 71、移动宽带 243/244，`FWPM_CONDITION_INTERFACE_TYPE`；这些类型里的虚拟网卡按 LUID 放行，蓝牙 PAN 仍拦）。**MagicDNS 已恢复**：连接时 `debian.tail707c29.ts.net -DnsOnly` 0.00 秒得到 `100.77.4.23`，`nohost-<随机>.tail707c29.ts.net` 0.00 秒得到 NXDOMAIN（改动前 12.03 秒超时）。
  - **NXDOMAIN 仍约 12 秒，未修好**。`Resolve-DnsName nonexistent-<随机>.example` 每种做法各 5–6 次：改动前（拦除 cellglide0 外所有网卡）12.02–12.07 秒；只拦物理网卡、`ALE_AUTH_CONNECT` 12.02–12.08 秒；只拦物理网卡、`OUTBOUND_TRANSPORT` 12.03–12.08 秒；最终版本 12.02–12.08 秒；断开时 0.02–0.05 秒。能查到的名字 0.01–0.12 秒。
  - 原因：用户态 WFP 没有任何办法让 UDP 发送立即失败。单独测套接字（硬拦截发往 `192.168.1.254:53`）：在 `ALE_AUTH_CONNECT`、`OUTBOUND_TRANSPORT`、`DATAGRAM_DATA` 三层，`sendto`（未连接和已连接、第一次和 300 ms 后的再次发送）都立即成功，接收都要等到超时；只有 TCP 在 `ALE_AUTH_CONNECT` 上 0.3–0.7 ms 就以 `WSAEACCES`（10013）失败（在 `OUTBOUND_TRANSPORT` 上 5 秒超时）。DNS 客户端用 UDP，并且用通配地址 `::` 绑定套接字，所以也没法按本地地址拦它的 bind。保留 `ALE_AUTH_CONNECT`：UDP 上不比其他层慢，TCP 能立即失败。
  - 泄漏仍为 0：连接时 pktmon 抓 `--comp nics`、端口 53，60 秒内 360 次查询、90 次 `.ts.net` 查询、90 次网页请求：Wi-Fi 0 包；只有 Tailscale Tunnel 有 Rx 180 / Tx 180（MagicDNS，符合预期）。对照（端口 443，5 秒）：Wi-Fi Rx 7 / Tx 7，CellGlide Tunnel Rx 14 / Tx 10。直接问 `192.168.1.254` 和 `2600:381:7368:4c73::1` 都超时，`100.100.100.100` 0.01 秒有应答。
  - 断开后 `netsh wfp show filters` 里 CellGlide 对象 0 个；快照与基线 `identical`。
- **测速上行是 0.0**，见第 8 项。W2 Task 4 已修（引擎按服务器确认的字节计上行，cg-netstack 在客户端 RST 后中止 SOCKS 一侧），见「上行 0.0 调查」。
