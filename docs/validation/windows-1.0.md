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

待测（需要用户在场，controller 稍后和用户一起做）。注意：用户执行过 `disconnect` 后，`user_disconnected` 会写进设置，自动连接要等下一次 `connect` 才恢复；测之前先 `cellglide.exe connect` 一次。

### 6. 睡眠 / 唤醒

待测（需要用户在场，controller 稍后和用户一起做）。

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

## 已知问题（W1 实测发现，未修）

- **严格 DNS 下，查不到的名字要等约 11 秒**。连接时 `Resolve-DnsName nx<随机>abc.com -DnsOnly` 要 11.2–11.4 秒才返回 NXDOMAIN，断开后只要 0.39 秒；直接问 `198.18.0.1` 只要 0.15–0.8 秒。原因是 Windows DNS 客户端收到一个网卡的否定应答后，还会等其他网卡的 DNS 服务器（Wi-Fi 的 `10.10.10.1`、Tailscale 等），而这些 53 端口的包都被 WFP 拦掉，只能等超时。能查到的名字不受影响（0.01–0.4 秒）。另外，Tailscale 的 MagicDNS（NRPT `.ts.net` → `100.100.100.100`）在连接期间无法使用：`nohost.tail707c29.ts.net` 11.4 秒后超时。
- **测速上行是 0.0**，见第 8 项。
