# Windows 1.0 W0 验证记录

## 机器与工具链

| 项目 | 值 |
| --- | --- |
| 系统 | Windows 11 专业版 23H2 (22631) x64 |
| CPU | 11th Gen Intel Core i7-1195G7 @ 2.90GHz |
| rustc | 1.99.0 (b940084d7 2026-09-28)，由 rust-toolchain.toml 固定 |
| Rust targets | x86_64-pc-windows-msvc、aarch64-pc-windows-msvc |
| clang | 19.1.5（VS 2022 BuildTools 的 Clang 组件，`VC\Tools\Llvm\x64\bin`） |
| MSVC | 14.44.35207 |
| .NET SDK | 10.0.403 |
| gh | 2.103.3（`C:\Program Files\GitHub CLI\gh.exe`），已登录 kikohz |

开发环境：每个新 PowerShell 会话先运行 `. scripts\dev-env.ps1`（把 cargo 和 clang 加到 PATH）。
clang 来自 `setup.exe modify --add Microsoft.VisualStudio.Component.VC.Llvm.Clang`；ring 的构建脚本编译 ARM64 目标时需要它。

## Task 1：编译基线

### cargo check

- `cargo check --workspace --locked`（x86_64）：Finished。
- `cargo check --workspace --locked --target aarch64-pc-windows-msvc`：Finished（安装 clang 之前因 ring 找不到 clang 失败）。

### 测试代码编译状态（Task 8 的清零清单）

当前 cargo 不支持 `--keep-going`，改为逐个 crate 运行 `cargo test -p <crate> --locked --no-run`。
只有 cg-core（及依赖其 testing 模块的 cg-service）测试编译失败，其余 crate 都能编译：

| 位置 | 错误 |
| --- | --- |
| crates/cg-core/src/testing/fake_mux.rs:9:29 | E0432 `tokio::net::UnixListener`、`UnixStream` 在 Windows 上不存在 |
| crates/cg-core/src/testing/fake_mux.rs:51:22 | E0599 `MuxEndpoint::Unix` 变体不存在 |
| crates/cg-core/src/mux.rs:231:33 | E0599 `MuxEndpoint::Unix` 变体不存在 |
| crates/cg-core/src/controller.rs:1769:26 | E0599 `MuxEndpoint::Unix` 变体不存在 |

cg-service 的测试编译失败同样来自 fake_mux.rs 的前两项（经 cg-core 的 testing 模块）。
这是编译器在第一阶段报出的错误；修掉后可能暴露更多，Task 8 完成时需重新运行并更新本节。

### GitHub

`gh` 已安装（2.103.3，`C:\Program Files\GitHub CLI\gh.exe`），`gh auth status` 显示已登录 github.com 账号 kikohz（https，scopes: gist、read:org、repo）。`gh auth setup-git` 已运行，`git push origin windows` 可用。（本会话的 PATH 过期曾导致误判未安装。）

## Task 3：wintun

### wintun.dll 来源

- 下载地址 `https://www.wintun.net/builds/wintun-0.14.1.zip`。
- SHA2-256 照抄自 https://www.wintun.net/ 下载链接旁的公布值（2026-10-29 抄录）：
  `07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51`，已写死在 `scripts/fetch-wintun.ps1`。下载后 `Get-FileHash` 一致。
- 另外核对了 zip 里 `bin\amd64\wintun.dll` 的 Authenticode 签名：`Get-AuthenticodeSignature` 为 `Valid`，签名者 `CN=WireGuard LLC`（颁发者 DigiCert EV Code Signing CA (SHA2)，证书指纹 `DF98E075A012ED8C86FBCF14854B8F9555CB3D45`）。
- 脚本把 DLL 复制为 `wintun.dll`，许可证复制为 `wintun-LICENSE.txt`（与设计 §5.2 的安装目录命名一致），默认目标 `target\debug`、`target\debug\deps`、`target\debug\examples`。下载物只在 `target\wintun\`。

### 加载

- `LoadLibraryExW` 带 `LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`；debug 版先看 `CELLGLIDE_WINTUN_DLL`（必须是绝对路径）。
- 找不到时的错误：`can't load wintun.dll from C:\nonexistent\wintun.dll (The specified module could not be found. (os error 126)); it should be in the same directory as cellglide-service.exe`。

### 特权测试（管理员）

`cargo test -p cg-platform --test windows_privileged --locked -- --ignored --test-threads=1`：3 个测试全部通过。

| 测试 | 结果 |
| --- | --- |
| adapter_appears_and_disappears | 创建后 `Get-NetAdapter -IncludeHidden -Name cellglide0` 能查到；drop 后第一次查询就查不到（约 0.63 s，基本是启动 powershell 的时间） |
| open_finds_an_existing_adapter | `Adapter::open` 返回 Some，LUID 与创建的一致；两者 drop 后 `open` 立即返回 None（< 1 ms） |
| session_starts_receives_and_sends | 4 MiB 会话可启动，读事件句柄有效，`try_receive` 正常，发送 20 字节 IPv4 包成功，空包被拒绝 |

前后快照 `-Compare`：`identical`。

### 崩溃清理（`wintun_probe` + `taskkill /F`）

- 运行中：`Get-NetAdapter` 显示 `cellglide0`，`InterfaceDescription` 为 `CellGlide Tunnel`，状态 Up；`pnputil /enum-devices /class Net` 里是 `SWD\Wintun\{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}`，Status `Started`。
- `taskkill /F` 之后：`Get-NetAdapter -IncludeHidden` 约 40 ms 内就查不到，`Get-NetIPInterface -InterfaceAlias cellglide0` 为空，快照对比 `identical`。
- **与预期不同**：`pnputil /enum-devices /class Net` 里**仍有**这一项，Status `Disconnected`；`Get-PnpDevice` 显示 `Present=False`、`Problem=CM_PROB_PHANTOM`。也就是说，网卡和它的 IP 接口随进程消失，但 PnP 设备记录以「不存在的设备」形式留下（同拔掉的 USB 设备）。`pnputil /enum-devices /connected` 里没有它。
- 这个残留记录：`WintunOpenAdapter("cellglide0")` 对它返回「找不到」（`Adapter::open` 为 None）；下一次正常的 create + close 之后设备记录完全消失（`Get-PnpDevice` 查无此项）。因为 GUID 固定，残留最多只有这一条，不会累积。
- 正常关闭（drop / `WintunCloseAdapter`）不留任何设备记录。
- 结论：崩溃后不需要额外清理网络配置；PnP 里的不存在设备记录在下次服务启动建网卡并关闭时被替换掉。是否要在卸载脚本里删掉这条记录，留给安装脚本任务决定（不在本任务里动）。

### 同名同 GUID 第二个实例

第一个 `wintun_probe` 持有网卡时再运行第二个：

- 第二个实例的 `WintunOpenAdapter("cellglide0")` **成功**（拿到同一 LUID `0x35008001000000`）。
- 它的 `WintunCreateAdapter("cellglide0", "CellGlide", 固定 GUID)` **失败**，last error 是 HRESULT `0x800700B7`（`HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`）而不是 Win32 码。封装里已把 `0x8007xxxx` 形式还原为 Win32 码，现在报 `os error 183`，`ErrorKind::AlreadyExists`。
- 第二个实例退出（关闭它打开的句柄）不影响第一个：第一个仍在运行，网卡仍 Up。杀掉第一个后网卡消失，快照对比 `identical`。

### 其他

- 本机另有一个不属于我们的 wintun 设备 `SWD\Wintun\{37217669-42DA-4657-A55B-0D995D328250}`（`Tailscale Tunnel`，正在运行，Present=True），全程未触碰。
- 所有快照对比（特权测试、两次崩溃实测、清理）均为 `identical`。

## Task 5：DNS 与绑定实测

示例：`crates/cg-platform/examples/dns_probe.rs`、`unicast_if_probe.rs`。全部在管理员 PowerShell 里运行，每次运行前后都做了 `scripts\network-snapshot.ps1` 快照对比，**全部为 `identical`**（共 14 次）。pktmon 过滤器用完即 `pktmon filter remove`，.etl/.txt 只在 `target\w0dns\`。

测试机网络：物理网卡只有 Wi-Fi（ifIndex 10，pktmon 组件 13 `Intel(R) Wi-Fi 6 AX201 160MHz`），IPv4 DNS `10.10.10.1`，IPv6 DNS `fd1e:f1ee:8b51::1`（ULA，同链路），**没有 IPv6 默认路由**。另有 Tailscale（未开出口节点；其 NRPT 只覆盖 ts.net 和 100.x 反向域）。系统没有 SMHNR 相关策略（`DisableSmartNameResolution` 等均未设置，即默认值）。

### DNS

#### 做法

`dns_probe` 建 cellglide0（地址 `198.18.0.1/32`、`fd6c:6779:6465::1/128`，两个协议族接口跃点 0），`set_dns([198.18.0.1])`，在 `198.18.0.1:53` 上起 UDP 假 DNS（同时绑 TCP）。**不加任何路由。** 只有 `probe-*` 名字按模式处理；系统自己的后台查询原样转发给 Wi-Fi 的 `10.10.10.1`，避免假应答进系统缓存。每个模式：

```powershell
pktmon filter remove; pktmon filter add -p 53
pktmon start --capture --comp nics --pkt-size 0 -f target\w0dns\pktmon-<模式>.etl
Resolve-DnsName "probe-$(Get-Random).example.com" -DnsOnly -QuickTimeout   # 5 次，名字每次不同
pktmon stop; pktmon etl2txt ... -o target\w0dns\pktmon-<模式>.txt
```

计数口径：pktmon 文本里组件 13（Wi-Fi 网卡）上 `Direction Tx`、内容含 `? probe-` 的包（每个 A / AAAA 查询各算一个）。

#### 结果

| 模式 | Wi-Fi 上的 probe 查询包 | `Resolve-DnsName` 结果与耗时 |
| --- | --- | --- |
| `--answer` | 8（5 个名字；有 2 个名字只发了 A） | 我们的 A `192.0.2.1`，1–39 ms |
| `--answer` 且 cellglide0 另设 IPv6 DNS `fd6c:6779:6465::1`（`--v6-dns`） | 9 | 我们的 A，0–29 ms |
| `--delay 500` | 10 | 我们的 A，约 505 ms |
| `--delay 800` | 10 | 我们的 A，约 805 ms |
| `--delay 1000` | 10 | 我们的 A，约 1005 ms |
| `--delay 1500` | 10 | 我们的 A，约 1505 ms |
| `--delay 3000` | 10 | 我们的 A，约 3005 ms |
| `--servfail` | 10 | **SOA**（即 Wi-Fi 上游回的 NXDOMAIN），39–92 ms |
| `--silent` | 10 | SOA（Wi-Fi 的 NXDOMAIN），约 7.0 s |
| `--silent`，不加 `-QuickTimeout`（2 个名字） | 4 | SOA，约 12.0 s |
| `--silent`，`--comp all`（2 个名字） | 4（网卡组件上） | SOA，约 7.0 s |

摘录（`--servfail`，一个名字，时间为本地时间）：

```text
00:00:31.261994 Tx Component 13 | fd1e:f1ee:8b51:0:9958:67c2:8e12:9521.61861 > fd1e:f1ee:8b51::1.53: 3877+ A? probe-1118483121.example.com. (46)
00:00:31.262097 Tx Component 13 | fd1e:f1ee:8b51:0:9958:67c2:8e12:9521.57183 > fd1e:f1ee:8b51::1.53: 24394+ AAAA? probe-1118483121.example.com. (46)
00:00:31.317072 Rx Component 13 | fd1e:f1ee:8b51::1.53 > ...57183: 24394 0/1/0 (108)
00:00:31.317073 Rx Component 13 | fd1e:f1ee:8b51::1.53 > ...61861: 3877 0/1/0 (108)
```

`--answer` 模式（立即回应）里同样每个名字都出现在 Wi-Fi 上，例如 `A? probe-1434172869.example.com.` 发往 `fd1e:f1ee:8b51::1.53`。

观察到的规律：

1. **每个查询在发给 cellglide0 的同时（0.1 ms 内）就并行发往 Wi-Fi 的 DNS 服务器**，不是超时之后才发。与我们怎么回（立即应答、延迟、SERVFAIL、不回）无关。这就是 Windows 的「智能多宿主名称解析」（SMHNR）并行查询。
2. Windows 对每块网卡只问一个服务器：Wi-Fi 上问的是 IPv6 的 `fd1e:f1ee:8b51::1`，`10.10.10.1` 从没收到 probe 查询；给 cellglide0 再设 IPv6 服务器后，我们的 IPv4 地址也不再收到查询，全部发到 `[fd6c:6779:6465::1]:53`。
3. Wi-Fi 的查询每个名字只发一次，之后的重试只发给我们：`-QuickTimeout` 时我们在 +1 s、+3 s 收到重发，7 s 放弃；不加时在 +1、+2、+4、+8 s 重发，12 s 放弃。重试阶段物理网卡上没有新增 probe 查询。
4. Windows **优先采用 cellglide0 的应答**：Wi-Fi 的 NXDOMAIN 几十毫秒就到了，但只要我们在等待期内回应（测到 3000 ms），结果就是我们的 A 记录。
5. **我们回 SERVFAIL 时，Windows 立即改用 Wi-Fi 的应答**；我们不回时，超时后也用 Wi-Fi 的应答。也就是说「期限到了先回 SERVFAIL」会让应用拿到物理网卡上游的结果。

注意：本测试按要求不加路由（加 /1 路由会断网）。实际隧道下 cellglide0 有四条 /1 路由，但同样没有 `0.0.0.0/0`；这一差别是否改变 SMHNR 行为没有实测。上面的现象与 research-net §3.1 引用的 Tailscale 注释（「Windows 8.1 及以后会并行向所有网卡的 DNS 服务器发请求」）一致。

#### 结论

- **SERVFAIL 后 Windows 会不会问别的网卡**：不需要等 SERVFAIL，**连立即回应的查询都已经并行发到物理网卡**；SERVFAIL 只会让 Windows 改用那份物理网卡的应答。
- **默认做法会泄漏，按 spec §7.3 需要回头问用户严格 DNS。**
- **`resolver_deadline`**：按「物理网卡上不出现 53 端口包的最大延迟减 200 ms」取值时，**没有任何延迟满足条件**（0 ms 也有包），公式无解。退一步按「除首发并行查询外，物理网卡上不再新增 probe 包的最大延迟减 200 ms」：测到的最大延迟 3000 ms 内都没有新增包，得 **2800 ms**（暂定）。期限对泄漏本身没有影响；它决定的只是 Windows 用谁的应答：期限内回应就用我们的，期限到了回 SERVFAIL 就用物理网卡的。
- `fec0:0:0:ffff::1-3`：本任务 12 次 `dns_probe` 测量运行中，cellglide0 的 IPv6 DNS 列表都是**空**（`Get-DnsClientServerAddress` 的 IPv6 项无地址），没有出现 Task 4 见过的 fec0 占位；所有抓包（含 `--comp all`）里 fec0 出现 0 次。建议：**无害，不需要处理**。显式设 IPv6 服务器 `fd6c:6779:6465::1` 也挡不住泄漏（上表 9 个包），反而要求解析器同时监听 `[fd6c:6779:6465::1]:53`，所以维持「网卡 DNS 只设 `198.18.0.1`」。
- cellglide0 的 `Get-NetAdapterBinding`（已启用）：`ms_lldp`、`ms_tcpip`、`ms_server`、`ms_lltdio`、`ms_tcpip6`、`ms_rspndr`、`ms_msclient`。与 Tailscale 网卡的已启用项完全相同，是 Windows 对新网卡的默认绑定，没有第三方过滤驱动等意外绑定。

#### 严格 DNS 复测（Task 11b，2026-10-30）

用户已同意严格 DNS。实现：`cg_platform::windows::strict_dns`，`FwpmEngineOpen0` 带 `FWPM_SESSION_FLAG_DYNAMIC`，一个事务里加子层（固定 GUID `{C151ED32-5A98-401F-B5CA-06705DB34EBA}`）和 `FWPM_LAYER_ALE_AUTH_CONNECT_V4` / `_V6` 上各三条规则：允许本地接口 == cellglide0 的 LUID（权重 15）、允许回环（权重 14）、拦截远端端口 53（不限协议，权重 0，硬拦截）。句柄关闭或进程结束，BFE 删除全部规则。

复测：`dns_probe --answer --strict`（`--strict` 在绑好套接字后开启严格 DNS；不带 `--upstream`），同一机器、同一 Wi-Fi，pktmon 命令与计数口径同上（5 个 `probe-*` 名字，`--comp nics`，过滤 `-p 53`）。同一会话里先后各跑一次有无严格 DNS 作对照：

| 模式 | 物理网卡上 53 端口的包（全部，含 Rx 和系统后台查询） | 其中 probe 查询包 | `Resolve-DnsName` |
| --- | --- | --- | --- |
| `--answer`（对照，无严格 DNS） | 28 | 9（发往 `fd1e:f1ee:8b51::1.53`） | 我们的 A `192.0.2.1`，1–199 ms |
| `--answer --strict` | **0** | **0** | 我们的 A `192.0.2.1`，1–201 ms |

另有特权测试（`crates/cg-platform/tests/windows_privileged.rs`）：

- `strict_dns_blocks_other_port_53_only_while_alive`：开启后向 Wi-Fi 的 `10.10.10.1` 和 `fd1e:f1ee:8b51::1` 发 UDP 查询 2 s 内都无应答（`send_to` 本身成功，包在 ALE 层被丢弃）；同时 TCP 连 `1.1.1.1:443` 成功、发往 `198.18.0.1:53` 的查询照常到达；drop 后同一查询立即有应答。
- `strict_dns_rules_vanish_when_the_process_dies`：子进程开启后被 `TerminateProcess`，Wi-Fi DNS 的查询在杀进程后约 3 ms 就恢复应答（两次运行 2.6 ms、2.9 ms）。
- `start_finish_stop_leaves_nothing`：真实隧道（加 /1 路由）下 `RunningTunnel::strict_dns()` 为真，Wi-Fi 的 `10.10.10.1`（同链路，不走隧道）查询被挡。

结论：严格 DNS 开启时，物理网卡上没有任何 53 端口出站，Windows 的并行查询被挡在本机；解析结果不受影响（Windows 采用 cellglide0 的应答）。测试结束后 `netsh wfp show filters` 和 `netsh wfp show state`（只读导出）里都找不到 CellGlide 的过滤器或子层 GUID。

### 绑定

| 场景 | 结果 |
| --- | --- |
| 无人占用 53 端口时绑 `198.18.0.1:53`（UDP、TCP） | 都成功（每次 `dns_probe` 运行都成功，共 14 次） |
| 另一进程先以 `SO_EXCLUSIVEADDRUSE` 绑 `0.0.0.0:53`（UDP、TCP 都绑并 listen），再绑 `198.18.0.1:53` | UDP、TCP **都失败，`WSAEACCES` 10013**（"An attempt was made to access a socket in a way forbidden by its access permissions"） |
| 我们先占 `198.18.0.1:53`，另一进程再以 `SO_EXCLUSIVEADDRUSE` 绑 `0.0.0.0:53` | 对方 UDP、TCP 都失败，`WSAEADDRINUSE` 10048；我们不受影响 |

本机 `SharedAccess`（ICS）和 `hns` 服务都是 Stopped / Manual，没有进程占用 53 端口。解析器绑定失败且错误码为 10013 时，可以判定为「有程序独占了 0.0.0.0:53」（典型是 ICS / Hyper-V 默认交换机），据此给出明确的原因。

### IP_UNICAST_IF

`unicast_if_probe`：先解析 `speed.cloudflare.com`（`162.159.140.220`、`2a06:98c1:58::da`），建 cellglide0，加 `0.0.0.0/1`、`128.0.0.0/1`、`::/1`、`8000::/1`，然后连 443。物理网卡按 `GetIpForwardTable2` 里排除 cellglide0 后（路由跃点 + 接口跃点）最小的默认路由选出：IPv4 为 ifIndex 10 `Wi-Fi`，IPv6 没有默认路由。

```text
[v4] plain connect 162.159.140.220:443: TIMED OUT after 3002 ms; packets to it in the tunnel: 2
[v4] UNICAST_IF=10 (Wi-Fi) connect 162.159.140.220:443: CONNECTED in 33 ms from 10.10.10.215
[v6] no physical default route: skipped
teardown: flush Ok(()), adapter closed, 3.8 s total
```

- 普通连接的 SYN（含一次重传）进了隧道，3 s 超时；设 `IP_UNICAST_IF`（ifIndex 的网络字节序）后 33 ms 连上，源地址是 Wi-Fi 的 `10.10.10.215`。**结论：`IP_UNICAST_IF` 能绕过 /1 路由。**
- `IPV6_UNICAST_IF`：本机没有 IPv6 上网，**未测**（代码路径已写好，值为主机字节序）。
- 断网约 3.5 s；结束后 `Test-NetConnection 1.1.1.1 -Port 443` 成功（经 Wi-Fi），cellglide0 和 /1 路由都已消失，快照对比 `identical`。
- 示例在每条退出路径上都会撤路由：正常结束和出错时先 `flush` 再关网卡；panic 钩子里 `flush` 后退出；看门狗 20 s 强制 `flush` 并退出；进程被杀时网卡随句柄关闭消失（Task 3 已测）。

## Task 6：iPhone（Apple Mobile Device）

**状态：端点部分已测；手机部分未测。** 测试时 Windows 的 PnP 里没有任何 Apple USB 设备（没有 `VID_05AC`），usbmuxd 的 `ListDevices` 返回空列表，所以 `ReadPairRecord`、lockdown、TLS、电量都没法测。需要用户确认手机已插好、屏幕已解锁后重测。

### 27015 是谁提供的

「Apple 设备」应用（商店版）**没有 Windows 服务**：不存在名为 `Apple Mobile Device Service` 的服务，机器上也没有任何名字或路径含 Apple 的服务、计划任务、`Run` 注册表项。Brief 里「查服务注册表 `ImagePath`」这一步对商店版不适用。

- 包：`AppleInc.AppleDevices` 1.1792.39681.0（x64），包家族名 `AppleInc.AppleDevices_nzyj5cx40ttqa`。
- 监听者：`AppleMobileDeviceProcess.exe`，路径 `C:\Program Files\WindowsApps\AppleInc.AppleDevices_1.1792.39681.0_x64__nzyj5cx40ttqa\`（版本号在路径里，更新会变），监听 `127.0.0.1:27015`（只绑回环，一个 Listen 套接字）。
- 它在**用户会话**（Session 1）里以当前用户身份运行，不是 SYSTEM，也不是服务。父进程是同目录的 `AppleMobileDeviceLauncher.exe`。测试时 Apple 设备的界面窗口并没有开，27015 照样在监听，所以**不需要打开应用窗口**。
- 自启动：清单里声明了 `windows.startupTask`（TaskId `AppleInc.AppleDevices.AppleMobileDeviceLauncher`，Executable `AppleMobileDeviceLauncher.exe`，Enabled=true）。当前用户的 AppModel 注册表里该任务 `State=2`（已启用）。所以它是**用户登录时由启动任务拉起**，不是开机就有。这就意味着：LocalSystem 的 CellGlide 服务在没有用户登录（登录前）时连不上 27015；用户登录之后才可用。
- 清单还带一条出站防火墙规则（`Apple Devices`）。
- 没有停止或卸载任何 Apple 的进程；也就没有测「杀掉进程后会不会被拉起」。
- 手机的配对记录目录 `C:\ProgramData\Apple\Lockdown` 存在，里面只有 `SystemConfiguration.plist`，**没有任何 `<UDID>.plist`**，也就是这台电脑还没有保存过任何手机的配对记录（与「手机没连上」一致）。

对 Task 13 doctor 的含义：检查「27015 可连」即可，不要找服务；要提示的是「用户已登录，且 Apple 设备应用已安装（包家族名如上）」，失败时的指引是「安装/打开 Apple 设备」。服务以 LocalSystem 运行，访问回环 TCP 没有权限问题，但要重试：用户登录前 27015 不存在。

### phone_probe

`cargo run -p cg-core --example phone_probe` 在 Windows 上**不需要改动就能编译、运行**（`MuxEndpoint::system()` 是 TCP 127.0.0.1:27015）。输出：

```text
usbmuxd: no matching USB iPhone
```

我又用原始 plist 报文直接发了一次 `ListDevices`，回复是 `DeviceList` 为空数组（`<array/>`），说明 27015 协议层正常，只是没有设备。

### 信任与配对结论（暂定，待手机在线后确认）

没有测到「信任」弹窗由谁触发。现在能确定的只有：Windows 上 usbmuxd 端点由商店版 Apple 设备应用的用户态进程提供，配对记录目录是 `C:\ProgramData\Apple\Lockdown\<UDID>.plist`，与 macOS/Linux 的做法同形（Task 7 之后的代码已按 `ReadPairRecord` 走 mux 协议）。

暂定方案：沿用「只读配对记录」（经 mux 的 `ReadPairRecord`），CellGlide 自己不发起配对；记录不存在时，界面和 doctor 引导用户「连接 iPhone、解锁，在手机上点信任；必要时打开 Apple 设备」。该方案需要用手机重测后再定稿：重测要覆盖「不开应用窗口拔插线是否弹信任」「打开 Apple 设备后是否弹」「`ReadPairRecord` 是否成功」「lockdown/TLS/diagnostics/电量」。
