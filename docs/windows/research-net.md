# CellGlide Windows（x64 / ARM64）网络层调研

日期：2026-10-22
范围：只读调研，未改任何代码。对照对象是 Linux 实现（`crates/cg-platform`、`crates/hev-sys`、`crates/cg-core/src/{tunnel,resolver,mux,forwarder}.rs`、`AGENTS.md`、`docs/specs/2026-10-15-linux-1.0-design.md`）。
标记说明：**[已核实]** 指读过一手源码或官方文档；**[待实测]** 指只有二手资料或推断，必须上真机验证。

---

## 0. 结论速览

| 问题 | 结论 |
|---|---|
| wintun 支持 ARM64 吗 | 支持。官方 zip 内含 amd64 / x86 / arm64 / arm 四种签名 DLL，当前版本 0.14.1 **[已核实]** |
| hev 能在 Windows 上用吗 | 上游只支持 **MSYS2 的 MSYS 环境（Cygwin 式 `msys-2.0.dll` 运行时）+ x86_64**。代码全部包在 `#if defined(__MSYS__)` 里，没有 MSVC，也没有 ARM64。MSVC / clang-cl / ARM64 移植 PR 截至今天**尚未合并** **[已核实]** |
| 推荐 TUN 引擎 | Windows 上先用 Rust 实现（tun2proxy 作库，或 ipstack + wintun-bindings），套在现有 `TunnelBackend` trait 后面；同时跟进 hev 的 MSVC PR，合并后再评估换回 hev |
| 手机路径需要绕过吗 | 不需要。usbmux 走 `127.0.0.1:27015`（AMDS），转发器和 SOCKS 都在 127.0.0.1；127/8 路由在环回接口上，比 /1 更具体。只有测速、上行比较这类工具要绑物理网卡（`IP_UNICAST_IF`） |
| DNS 推荐 | 默认只在 wintun 接口上设 DNS（`SetInterfaceDnsSettings`），关掉该接口的自动跃点并设跃点 0，解析器必须快速回应（失败也回 SERVFAIL，不能沉默）。可选的「严格 DNS」开关用 **WFP 动态会话**拦截其他 53 端口流量（进程一死，内核自动删规则）。**不默认用 NRPT**：NRPT 是系统级注册表配置，进程崩溃后仍会留着 |
| 服务账户 | 必须是 LocalSystem（或管理员）。wintun 驱动对 `TUN_IOCTL_REGISTER_RINGS` 做 `SeAccessCheck`，只放行 SY 和 BA；IP Helper 写路由也要求管理员。虚拟服务账户不够 |
| IPC | 用命名管道 + SDDL，按连接取客户端 PID 和令牌做授权。不要用 AF_UNIX：Windows 上没有 SO_PEERCRED，tokio 也不原生支持 |
| GUI | iced 0.14 自带 wgpu（DX12）和 tiny-skia 软件渲染，tray-icon 0.26 支持 Windows。ARM64 上 GPU 路径要实测 |
| 安装 | PowerShell 下载的文件没有 Mark-of-the-Web；由 SCM 启动的服务不经 SmartScreen；但 Windows 11 的 Smart App Control（强制模式）会拦下未签名的 exe，和 MotW 无关 |
| 构建 | `aarch64-pc-windows-msvc` 自 Rust 1.91 起是 Tier 1；GitHub 有 `windows-11-arm` 运行器（私有仓库按分钟计费）；也可以在 x64 上用 VS 的 ARM64 构建工具交叉编译 |

---

## 1. TUN

### 1.1 wintun：ARM64 与分发许可

- wintun.net 的发布包是一个 zip，内含 **AMD64、X86、ARM64、ARM32** 四种已签名的 `wintun.dll`，当前版本 0.14.1。官网称这些签名 DLL 是 "the only supported way of distributing Wintun"。**[已核实]** https://www.wintun.net/
- 用法：把对应架构的 `wintun.dll` 放在程序旁边，用 `LoadLibraryEx` + `GetProcAddress` 动态加载，附带 `wintun.h`。README 要求不要自己分发名为 "Wintun" 的其他文件，以免与官方部署冲突。**[已核实]** https://git.zx2c4.com/wintun/tree/README.md
- 源码是 GPL-2.0；预编译 DLL 用单独的 **Prebuilt Binaries License**（`prebuilt-binaries-license.txt`，也在 zip 里）。要点 **[已核实]**（https://github.com/WireGuard/wintun/blob/master/prebuilt-binaries-license.txt）：
  - 不得反编译或修改 DLL（3a、3b）。
  - 不得再分发，「除非 Software 与其他只通过 Permitted API（`wintun.h`）调用它的软件一起分发」（3d）。我们原样放在安装包里、只通过 `wintun.h` 的 API 调用，正好符合这一例外。
  - 未经书面同意，不得用 WireGuard / Wintun 的名义为产品背书（3e）。官网和商店文案不要写「基于 WireGuard 技术」之类的话。
  - 安装目录里保留许可证文本，并在「关于 / 开源许可」页列出。
- API 要点 **[已核实]**（README 与 `api/adapter.c`）：
  - `WintunCreateAdapter(Name, TunnelType, RequestedGUID)`：GUID 传 NULL 时每次随机，会「为每个新适配器新建一条 NLA 记录」。我们应传**固定 GUID**，这样网络位置（NLA）记录、`Tcpip\Parameters\Interfaces\{GUID}` 注册表键都只有一份，不会越积越多。
  - `WintunCloseAdapter`：若适配器是本进程创建的，关闭时一并删除。
  - 0.14 内部用 `SwDeviceCreate` 创建软件设备，**没有**调用 `SwDeviceSetLifetime`。微软文档写明默认生命周期是 `SWDeviceLifetimeHandle`：句柄关闭后 PnP 就不再枚举该设备，进程退出（含崩溃）时句柄随之关闭。另外，wintun 每次 create / open / close 时会排队执行 `AdapterCleanupOrphanedDevices`，清理带问题标记的残留设备节点。https://learn.microsoft.com/en-us/windows/win32/api/swdevice/nf-swdevice-swdevicesetlifetime
  - 结论：和 Linux 关闭 fd 即删除接口的语义基本一致。适配器随进程消失，挂在它 LUID 上的地址和路由也随之消失。**[待实测]**：`kill -9` 等效操作（`taskkill /F`）之后，用 `Get-NetAdapter -IncludeHidden` 确认没有残留。
  - 加载 DLL 时用 `LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`，防止 DLL 劫持。Program Files 默认只有管理员可写，安装目录 ACL 保持继承即可。

### 1.2 hev-socks5-tunnel 在 Windows 上的状态

已读本仓库子模块（2.18.0）源码，并查了上游（最新 2.19.1，2026-10-19）。**[已核实]**

- 有 `src/hev-tunnel-windows.c`、`src/misc/hev-wintun.c`，但整个文件包在 `#if defined(__MSYS__)` 里。
- Makefile 用 `ifeq ($(MSYSTEM),MSYS)` 链接 `-lmsys-2.0 -lws2_32 -lIphlpapi`。hev-task-system 的 IOCP reactor（`hev-task-io-reactor-iocp.c`）同样要求 `__MSYS__`。
- 也就是说，它针对的是 **MSYS2 的 MSYS 环境**（Cygwin 派生的 POSIX 运行时 `msys-2.0.dll`），不是 MinGW-w64 / UCRT64 / CLANG64，更不是 MSVC。产出的程序依赖 `msys-2.0.dll`。
- 协程上下文切换的汇编：Windows 下只有 `hev-task-execute-x86_64.s` 处理了 `__MSYS__`；aarch64 的汇编没有 Windows ABI 版本。
- MSYS2 官方说明 ARM64 上 "All unixy tools, like bash, will go through x64 emulation"。MSYS 运行时在 ARM64 上只能跑在 x64 仿真里，没有原生版本。https://www.msys2.org/docs/arm64/
- 上游发布物里只有 `hev-socks5-tunnel-win64.zip`（x64，MSYS2 构建），没有 Windows ARM64 包。
- Windows 版 hev **自己创建 wintun 适配器**（`hev_tunnel_open(name, multi_queue, guid)` → `WintunCreateAdapter`），不接受外部传入的 fd。这和 Linux 上「后端开 fd、交给 hev」的分工不同。
- 相关 issue / PR：
  - #156 作者答复：（当时）只支持 WSL2。https://github.com/heiher/hev-socks5-tunnel/issues/156
  - #320 作者答复："hev-socks5-tunnel depends on MSYS2"，同意别人去掉这一依赖。https://github.com/heiher/hev-socks5-tunnel/issues/320
  - **PR #337 "Add MSVC support"**：2026-10-17 提交，**OPEN**，约 +1252 行。https://github.com/heiher/hev-socks5-tunnel/pull/337
  - **hev-task-system PR #17 "Add native Windows MSVC support for x64, x86, and ARM64"**：2026-10-12 提交，**OPEN**。作者已转向 clang-cl 方案，维护者问「只支持 MinGW 是否更简单」，尚无定论。https://github.com/heiher/hev-task-system/pull/17
- 结论：**眼下 hev 无法以静态库形式链进 MSVC 构建的 Rust 服务，ARM64 上也没有原生版本。** 可选做法：
  1. （不推荐）随包带上上游的 x64 MSYS2 版 `hev-socks5-tunnel.exe` + `msys-2.0.dll`，作为子进程运行。ARM64 上走 x64 仿真，多一个进程，崩溃清理也更复杂。
  2. （推荐，第一阶段）Windows 用 Rust 用户态协议栈，见 1.3。
  3. （跟进）等 #337 和 hev-task-system #17 合并后，用 clang-cl 构建 hev，在 `hev-sys/build.rs` 里加 Windows 分支。届时再评估是否换回 hev，以保持与 Mac / Linux 一致（AGENTS.md 要求隧道常量相同）。

### 1.3 Rust 替代方案

| 方案 | 状态 | 说明 |
|---|---|---|
| **tun2proxy**（MIT，v0.8.6，2026-10-20） | 活跃，1.4k★ | 支持 Linux / macOS / Windows / iOS / Android；发布物里就有 `aarch64-pc-windows-msvc`、`x86_64-pc-windows-msvc`。支持 SOCKS5 UDP，也就是我们转发器已经实现的 UDP ASSOCIATE。依赖 `ipstack` 1.0（用户态 TCP/IP）和 `tun` 0.8.14（rust-tun，Windows 走 `wintun-bindings`）。lib 的 crate-type 含 `lib`，可以作库用；需要关掉它自带的路由和 DNS 功能（`--setup`、virtual DNS），只用它的转发部分。https://github.com/tun2proxy/tun2proxy |
| **tun-rs**（Apache-2.0，2.8.13） | 活跃 | 跨平台 TUN 库，Windows 需要 wintun.dll。只管设备，不含协议栈。https://github.com/tun-rs/tun-rs |
| **ipstack**（v1.0.1） | 活跃 | tun2proxy 的协议栈，可直接用。https://github.com/narrowlink/ipstack |
| **netstack-smoltcp**（v0.1.4，2025-07） | 更新较慢 | 基于 smoltcp。https://github.com/automesh-network/netstack-smoltcp |
| **netstack-lwip**（eycorsican） | 需要 C 编译 lwIP | 与 hev 同样基于 lwIP，Windows 上可以用 MSVC 编译。https://github.com/eycorsican/netstack-lwip |

建议：Windows 后端先用 **tun2proxy 作库**（或者直接用 ipstack + wintun-bindings 自己拼），挂在 `TunnelBackend` 后面。要对齐 hev 的参数：MTU 1500、TCP 读写超时 1800 s、UDP 空闲 30 s、最大会话数。另外要建一个与 Mac / Linux 共用的回归用例：同一组流量（TCP、UDP、DNS over UDP 和 TCP）在三个平台上的表现要一致。**[待实测]**：ipstack 的吞吐和 CPU 占用与 hev 的对比，尤其在 ARM64 上。

---

## 2. 路由

### 2.1 WireGuard / Tailscale 的默认路由做法 **[已核实]**

- **WireGuard for Windows**（`tunnel/addressconfig.go`）：
  - 路由来自 AllowedIPs，每条 `RouteData{Destination, NextHop: 0.0.0.0/::, Metric: 0}` 用 `luid.SetRoutesForFamily` 写到**自己的 LUID** 上。winipcfg 底层就是 `CreateIpForwardEntry2`。
  - 只有存在默认路由（/0）时，才对**自己的接口**设 `UseAutomaticMetric = false; Metric = 0`。同时设 `RouterDiscoveryDisabled`、`DadTransmits = 0`，并清掉 managed / other-stateful 标志。
  - 不修改其他网卡的设置。地址冲突时只删除**已断开**的接口上的同名地址（`cleanupAddressesOnDisconnectedInterfaces`）。
  - https://github.com/WireGuard/wireguard-windows/blob/master/tunnel/addressconfig.go
- **Tailscale**（`wgengine/router/osrouter/ifconfig_windows.go`）：同样只在自己的接口上设 `UseAutomaticMetric=false; Metric=0`，并设 `DadTransmits=0` 和 `RouterDiscoveryDisabled`。https://github.com/tailscale/tailscale/blob/main/wgengine/router/osrouter/ifconfig_windows.go
- **CreateIpForwardEntry2**：路由的完整跃点数 = 路由自身的 Metric 偏移 + 接口跃点数；函数要求管理员权限，否则返回 `ERROR_ACCESS_DENIED`。https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-createipforwardentry2
- **我们的做法**（与 Mac / Linux 常量一致）：在 wintun LUID 上加 `0.0.0.0/1`、`128.0.0.0/1`、`::/1`、`8000::/1`，下一跳为未指定地址。按最长前缀匹配，它们胜过任何 `/0` 默认路由，**不需要改其他网卡的跃点，也不删别人的默认路由**。我们接口的跃点仍设为 0：一是让 DNS 首选我们的接口（见 §3），二是与 WireGuard / Tailscale 的做法一致。这些设置只改我们自己的接口，符合 AGENTS.md。
- IPv6：Windows 的 IPv6 也要加 /1 路由，否则 IPv6 流量会从物理网卡直接出去（等于泄漏）。地址加不上时（例如 IPv6 被禁用）按 Linux 的做法降级成仅 IPv4，并记录下来。

### 2.2 手机 / usbmux 路径是否要绕过 **[已核实（本仓库代码）]**

- `cg-core/src/mux.rs`：非 Unix 平台使用 `MuxEndpoint::Tcp(127.0.0.1:27015)`，即 Apple Mobile Device Service（AMDS）。
- `forwarder.rs` 监听 `127.0.0.1:0`，hev（或替代方案）连的 SOCKS 地址是 `127.0.0.1`；DoH 也经 usbmux 走环回。
- Windows 上 `127.0.0.0/8` 路由在 Loopback Pseudo-Interface 上，/8 比 /1 更具体，环回流量永远进不了 TUN。手机路径完全走 USB，不经过 IP 路由。**结论：手机路径不需要任何绕过路由。**
- 仍然需要处理的：
  1. **测速 / 上行比较工具**（`cg-tools/src/uplink.rs`、`speed`）。Linux 读 `/proc/net/route`；Windows 用 `GetIpForwardTable2` 找出排除 wintun LUID 后跃点最小的 /0，再在 socket 上设 `IP_UNICAST_IF` / `IPV6_UNICAST_IF` 绑到该接口。WireGuard-go 的 Windows bind 也是这样避免回环的。https://learn.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options
  2. **局域网绕过**（Linux §3.4）：Windows 上本网段的 on-link 路由（如 `192.168.1.0/24`）和 `224.0.0.0/4` 多播路由本来就挂在物理网卡上，前缀都比 /1 长，**无需额外添加就会留在本地**。只有「经网关可达的其他私有网段」（例如 `10.0.0.0/8` 里非本网段的部分）需要绕过路由。这类路由要写到**物理网卡的 LUID** 上，属于在别的接口上加东西。Linux 那边也是这么做的，并用 realm 198 标记。Windows 的 `MIB_IPFORWARD_ROW2` 没有 realm 字段，只能用独特的 Metric 值（如 32766）作标记，再把加过的路由记到 `%ProgramData%\CellGlide\routes.json`。建议 Windows 第一版**只依赖 on-link 路由，不加经网关的私有网段路由**，待确有需求再做，且要同样遵守「只删自己加的」。
  3. 进程内部的连接（健康检查、诊断上传）本就应该走隧道或环回，不需要处理。

### 2.3 崩溃后的清理

| 对象 | 崩溃后的状态 | 处理 |
|---|---|---|
| wintun 适配器 | SwDevice 句柄随进程关闭，适配器消失 **[已核实，待实测]** | 服务启动时用固定名称 / GUID 调 `WintunOpenAdapter`，若能打开说明有残留，先关掉 |
| wintun LUID 上的地址和路由 | 随接口消失。WireGuard 在删除接口前会主动 `FlushRoutes`、`FlushIPAddresses`、`FlushDNS`（`interfacewatcher.go`），因为 "the Windows networking stack doesn't like it when we destroy interfaces that have active routes" | 正常停止时照做 |
| 接口 DNS（注册表 `Tcpip\Parameters\Interfaces\{固定 GUID}`） | 键会保留，但只对这个 GUID 的接口生效，接口不在时无影响 | 停止时清空；卸载时删除整个键 |
| WFP 动态会话的过滤规则 | 会话结束时自动删除（"any objects added during the session are automatically deleted when the session ends"）。https://learn.microsoft.com/en-us/windows/win32/api/fwpmtypes/ns-fwpmtypes-fwpm_session0 | 不需要处理 |
| NRPT 规则 | **会保留**（Tailscale 源码注释："NRPT rules survive the unclean termination"），Tailscale 每次启动先 `DelAllRuleKeys()` | 我们默认不用 NRPT |
| 物理网卡上的绕过路由（若做） | 活动存储里的路由会一直留到重启或该接口复位 | 启动时按台账核对，只删自己加的 |
| 服务进程本身 | — | `sc.exe failure CellGlide reset= 86400 actions= restart/2000/restart/5000/restart/30000`，相当于 systemd 的 `Restart=on-failure` |

---

## 3. DNS：不泄漏，也不动其他网卡

### 3.1 Windows 的行为 **[已核实]**

- Policy CSP 对 "Turn off smart multi-homed name resolution"（`DisableSmartNameResolution`）的说明：默认会 "issuing parallel DNS, link local multicast name resolution (LLMNR) and NetBIOS over TCP/IP (NetBT) queries across all networks"。https://learn.microsoft.com/en-us/windows/client-management/mdm/policy-csp-admx-dnsclient
- VPN 名称解析文档给出的顺序：先查 NRPT；没有匹配时，只把查询发给**接口跃点最优**的那个接口；"If the query times out ... DNS queries are sent on all interfaces"。https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/vpn/vpn-name-resolution
- DNS 客户端超时序列：1 s 后重试，到第 4 s 时同时问所有服务器，第 10 s 放弃。"Any Name Error response ... will cause the process to stop"，即否定应答会立即终止。https://learn.microsoft.com/en-us/troubleshoot/windows-server/networking/dns-client-resolution-timeouts
- Tailscale 源码注释：即使开了 "Override local DNS"，"Windows 8.1 and newer devices issue parallel DNS requests to DNS servers associated with all network adapters ... resulting in DNS leaks"。因此 Tailscale 在全局 DNS 模式下额外写一条 `.` 的 NRPT 规则。https://github.com/tailscale/tailscale/blob/main/net/dns/manager_windows.go
- **Windows 的接口 DNS 不能指定端口**（`DNS_INTERFACE_SETTINGS.NameServer` 只是 IP 列表，NRPT 的 `GenericDNSServers` 也一样）。所以 Linux 上的 `127.0.0.1:1053` 在 Windows 上不能用，解析器必须监听 **:53**。建议监听 wintun 自身地址 `198.18.0.1:53`：本机发往本机地址的流量走环回，不会进 TUN，也不会和别人的 `127.0.0.1:53` 冲突。Linux 代码注释里已经把 `198.18.0.1:53` 列为备选。**[待实测]**：ICS / Hyper-V 默认交换机的 `SharedAccess` 若用 `SO_EXCLUSIVEADDRUSE` 绑了 `0.0.0.0:53`，我们绑具体地址是否会失败；失败时要给出明确的原因代码。

### 3.2 三种做法对比

| 做法 | 改了什么 | 防泄漏效果 | 崩溃后 | 是否符合「只改自己的接口」 |
|---|---|---|---|---|
| **A. 只在 wintun 上设 DNS + 该接口跃点 0** | 只改我们的接口：`SetInterfaceDnsSettings`（Win10 19041+），较老系统写 `Tcpip\Parameters\Interfaces\{GUID}\NameServer`。另外在我们的接口上关 LLMNR 和 NetBIOS（Tailscale 也这样做） | 正常情况下查询只发给首选接口，也就是我们。我们**超时**或系统做 SMHNR 并行查询时，仍可能发到其他网卡 | 接口消失，设置随之失效 | ✅ 完全符合 |
| **B. NRPT `.` 规则**（Tailscale） | 系统级注册表 `HKLM\SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig\{GUID}`；域环境下若已有 GP 版 NRPT，本地规则会被忽略，Tailscale 只好去写 GP 路径 `SOFTWARE\Policies\...` | 强：所有名字都发往指定服务器 | **会残留**。残留期间如果解析器不在，**整台机器的 DNS 都会失效** | ❌ 改的是系统级配置，而且不靠接口生命周期清理，与 Linux 侧「接口一消失就什么都不留」的原则冲突 |
| **C. WFP 拦截其他 DNS**（WireGuard 的 blockDNS 和 sing-box 的 `strict_route`） | 一个 `FWPM_SESSION_FLAG_DYNAMIC` 会话：拦截所有 UDP / TCP 53 端口出站，只放行发往我们解析器地址的流量；WireGuard 还放行环回和自身进程 | 强：其他网卡上的 53 端口直接被挡掉 | 会话随进程自动删除 ✅ | ⚠️ 不改任何网卡配置，但效果是系统级的：会挡住其他 VPN 的 split DNS、公司内网 DNS、只认 53 端口的本地工具。sing-box 文档也提到它 "may prevent some Windows applications (such as VirtualBox) from working properly" |

参考：
- WireGuard 只在「单 peer 且 AllowedIPs 含 /0」时才启用防火墙（`enableFirewall` 里的 `doNotRestrict`），规则见 `firewall/rules.go` 的 `blockDNS`。https://github.com/WireGuard/wireguard-windows/blob/master/tunnel/firewall/blocker.go
- sing-box tun 文档：https://sing-box.sagernet.org/configuration/inbound/tun/

### 3.3 建议

1. **默认用 A**，再补三条：
   - 解析器在 Windows 上**绝不能沉默**。上游（DoH）失败时 1 s 内回 SERVFAIL，不要让 Windows 等到「超时后问所有接口」那一步。现有 `UPSTREAM_TIMEOUT = 10s` 需要按平台调短，或者先回 SERVFAIL 再在后台继续。**[待实测]**：Windows 收到 SERVFAIL 后是否还会去问其他接口。微软文档只说 Name Error（NXDOMAIN）会立即终止。
   - 接口 DNS 只填一个地址（`198.18.0.1`，加上 IPv6 地址），不要填多个，避免触发「同时问所有服务器」。
   - 用 `pktmon` 或 Wireshark 抓物理网卡上的 53 端口，作为「DNS 泄漏页」的验收手段。
2. **「严格 DNS」作为可选开关，用 C**：动态 WFP 会话，进程崩溃即失效，不留任何持久状态。界面上要写明「会阻止其他程序使用自己的 DNS」。
3. **不用 B**。NRPT 是系统级、持久的配置，崩溃后会留下来，而且残留期间会让整台机器无法解析域名。这超出了 AGENTS.md「只改自己的接口」的边界。如果日后实测证明 A 有严重泄漏而 C 又不可接受，再单独评审 NRPT：必须有启动时清理、卸载时清理，并在 AGENTS.md 里写明豁免。
4. **系统级仍会变化的东西**（A 方案下）：
   - 多出一个网络适配器和一条 NLA 网络配置记录。使用固定 GUID 时只会有一条，网络类型可能显示为「公用 / 未识别网络」，这也是系统级可见的变化。
   - `Tcpip\Parameters\Interfaces\{我们的 GUID}` 注册表键，只属于我们，卸载时删除。
   - 如果做了局域网绕过路由，物理网卡上会多出我们的路由（见 §2.2）。
5. **卸载时保证清理**：停服务 → 等适配器消失（`Get-NetAdapter -Name cellglide0` 查不到）→ 删除我们 GUID 的 Tcpip 接口键 → 按台账删除绕过路由 → 删除服务。若曾经写过 NRPT（不建议），按 Tailscale 的方式删除 `DnsPolicyConfig\{我们的 GUID}`。
6. **验收**（对应 AGENTS.md「其他网卡 DNS 前后一致」）：安装和测试前后各导出一次 `Get-DnsClientServerAddress`、`Get-DnsClientNrptRule`、`Get-NetIPInterface | select ifAlias,InterfaceMetric,AutomaticMetric`，以及 `HKLM\...\Tcpip\Parameters\Interfaces` 下除我们 GUID 外的所有键，逐项比对。

---

## 4. 服务模型与权限

- **wintun 需要什么权限** **[已核实]**：驱动（`driver/wintun.c`）在处理 `TUN_IOCTL_REGISTER_RINGS`（启动会话）时用安全描述符 `O:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)S:(ML;;NWNRNX;;;HI)` 做 `SeAccessCheck`，即只放行 **LocalSystem 和内置 Administrators（高完整性级别）**。DLL 的 `main.c` 也会判断自己是否是 LocalSystem，据此为命名空间对象选择 SDDL。创建适配器要装驱动和建设备节点，同样需要管理员权限。早年邮件列表里作者还说过要锁定为仅 SYSTEM，后来放宽到管理员。https://lists.zx2c4.com/pipermail/wireguard/2019-May/004203.html
- **IP Helper 写路由、地址、接口跃点**都要求管理员（见 CreateIpForwardEntry2 文档）；WFP 添加过滤规则也要管理员。
- **结论**：虚拟服务账户（`NT SERVICE\CellGlide`）、LocalService、NetworkService 都不是管理员，**无法**运行 wintun。服务必须以 **LocalSystem** 运行，WireGuard 的隧道服务和 tailscaled 都是这样。可以采取的收紧措施：
  - `sc.exe privs CellGlide <列表>`（`SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO`）去掉用不到的特权。**[待实测]**：最小集合至少要包含 `SeLoadDriverPrivilege`（首次安装驱动）、`SeChangeNotifyPrivilege`、`SeImpersonatePrivilege`（如果用 ImpersonateNamedPipeClient）。https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_required_privileges_infow
  - `sc.exe sidtype CellGlide unrestricted`，给服务一个独立的服务 SID，方便为 `%ProgramData%\CellGlide` 设 ACL（只允许 SYSTEM、Administrators 和该服务 SID 写入）。
  - 服务只做网络相关的工作；诊断上传、HTTPS 测速这类需要联网的事交给用户进程（Linux 设计 §3.7 已经这样分工）。
- 服务账户说明：https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts

---

## 5. 服务与 GUI 之间的 IPC

| | 命名管道 + SDDL | AF_UNIX（Win10 1803+） |
|---|---|---|
| tokio 支持 | `tokio::net::windows::named_pipe`，原生支持；`ServerOptions::reject_remote_clients(true)`（默认），`first_pipe_instance(true)` 防止别人抢先建同名管道 | tokio 不支持，需要 `uds_windows` 之类的第三方 crate |
| 访问控制 | SDDL 直接写在管道上 | 靠 socket 文件和所在目录的 ACL |
| 识别对方 | `GetNamedPipeClientProcessId` → `OpenProcessToken` → `TokenUser` / `TokenGroups` / 会话 ID；或者 `ImpersonateNamedPipeClient` | 没有 SO_PEERCRED，不支持辅助数据（"There is no support for ancillary data"）。只有 mingw 头文件里定义了 `SIO_AF_UNIX_GETPEERPID`，微软没有公开文档 |
| 参考 | Tailscale 用 `D:P(A;;GWGR;;;BU)(A;;GWGR;;;SY)`，并刻意不设 owner（否则非管理员进程无法监听）；按连接取客户端 PID 判断身份。https://github.com/tailscale/tailscale/blob/main/safesocket/pipe_windows.go | https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/ |

**建议**：命名管道 `\\.\pipe\CellGlide\control`，SDDL 设为 `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`，即只允许 SYSTEM、管理员和**交互式登录用户**（IU），远程和服务账户进不来。每个连接建立时取客户端令牌，按请求类型授权，对应 Linux 上「root、cellglide 用户、cellglide 组成员」的规则：
- 只读请求（状态、记录）：任何 IU 用户都可以。
- 改变状态的请求（连接、断开、设置）：管理员，或者令牌会话 ID 等于当前活动控制台会话（`WTSGetActiveConsoleSessionId`）的用户。这样可以挡住快速切换用户后另一个会话里的程序。
- GUI 需要知道「当前用户是谁」时，不要相信客户端自己报的身份，一律以服务端从令牌里读到的 SID 为准；服务在 `hello` 回复里把这个 SID 告诉 GUI。

---

## 6. 托盘、自启动与 GUI

- **iced 0.14**（2025-12-07 发布）默认特性包括 `wgpu`（Windows 上走 DX12）和 `tiny-skia`（软件渲染）。iced 用的是 wgpu 27、winit 0.30。wgpu 曾有一个 issue：其示例在 `aarch64-pc-windows-msvc` 上因 `static-dxc` 失败（#9784，已关闭），说明 ARM64 + DX12 这条路有人在用，但个别特性会出问题。**[待实测]**：在 Snapdragon X 机器上跑一遍，GPU 路径不行时确认能回落到 tiny-skia。https://github.com/gfx-rs/wgpu/issues/9784
- **tray-icon 0.26.1**（2026-10-07）支持 Windows、macOS 和 Linux（AppIndicator 或 KSNI）。Windows 上要求托盘所在线程有事件循环在跑；iced / winit 的主线程满足这一条，事件通过 `set_event_handler` 转给 `EventLoopProxy`。这个 crate 是纯 Win32 实现，没有架构相关代码，ARM64 理论上没有问题。**[待实测]**。另外 tray-icon 现在也有 KSNI 后端，日后 Linux 端的 `ksni` 有机会统一换成它。https://github.com/tauri-apps/tray-icon
- **自启动**：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下写 `CellGlide = "C:\Program Files\CellGlide\cellglide-gui.exe" --tray`。用户在「设置 → 应用 → 启动」里关掉后，系统会写 `...\Explorer\StartupApproved\Run`，我们不要去覆盖。「登录后显示托盘」开关就是写入或删除这个值，对应 Linux 的 XDG autostart。https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys
- **睡眠 / 唤醒**（对应 logind 的 `PrepareForSleep`）：服务注册时声明 `SERVICE_ACCEPT_POWEREVENT`，在 `HandlerEx` 里接收 `SERVICE_CONTROL_POWEREVENT`，wParam 为 `PBT_APMSUSPEND` 或 `PBT_APMRESUMEAUTOMATIC`。系统大约给两秒处理 `PBT_APMSUSPEND`。`windows-service` 0.8.1（mullvad）已经封装好了 `ServiceControl::PowerEvent(PowerEventParam::Suspend | ResumeAutomatic | …)` 和 `ServiceControlAccept::POWER_EVENT`。https://learn.microsoft.com/en-us/windows/win32/power/pbt-apmsuspend 、https://github.com/mullvad/windows-service-rs
  - **风险**：Modern Standby（S0ix，ARM64 笔记本基本都是）下，有社区报告说收不到 `PBT_APMSUSPEND`，微软没有明确说明。**[待实测]**。兜底办法：同时订阅 `GUID_CONSOLE_DISPLAY_STATE` 等电源设置通知；健康判定里检测单调时钟与墙钟的跳变，跳变时把这段空档视为睡眠，与 `sleep.rs` 的目的一致。

---

## 7. 不做 MSI 的安装与卸载

### 7.1 PowerShell 安装脚本要点

```powershell
#Requires -RunAsAdministrator
$ErrorActionPreference = 'Stop'
$PSDefaultParameterValues['Invoke-WebRequest:UseBasicParsing'] = $true   # 5.1 在 2025-12 之后会弹安全确认，见下文
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
# 1 下载 zip 与 SHA256SUMS（只认 https://cellglide.com/... 这一个来源）
# 2 用 Get-FileHash -Algorithm SHA256 校验，不一致就中止
# 3 如有旧版：Stop-Service CellGlide，等 Get-NetAdapter -Name cellglide0 消失（最多约 10 s）再替换（AGENTS.md 的维护栅栏）
# 4 Expand-Archive 解到临时目录，再整体复制到 "$env:ProgramFiles\CellGlide"
# 5 New-Service -Name CellGlide -BinaryPathName '"C:\Program Files\CellGlide\cellglide-service.exe"' -StartupType Automatic
#    （路径必须加引号，避免「未加引号的服务路径」漏洞）
#    sc.exe failure / sidtype / privs 见 §2.3、§4
# 6 用 New-Item 建 %ProgramData%\CellGlide，并用 icacls 设显式 ACL（ProgramData 默认允许 Users 创建子项，必须收紧）
# 7 用 WScript.Shell COM 创建开始菜单快捷方式：$env:ProgramData\Microsoft\Windows\Start Menu\Programs\CellGlide.lnk
# 8 卸载：Stop-Service → 等适配器消失 → sc.exe delete → 删除快捷方式、Program Files 目录、我们 GUID 的 Tcpip 键、HKCU Run 值（逐个用户）；%ProgramData%\CellGlide 只在 -Purge 时删除
```

- **2025-12 起的变化** **[已核实]**：Windows PowerShell 5.1 的 `Invoke-WebRequest` 在没有 `-UseBasicParsing` 时会弹安全确认（默认选「否」），非交互脚本会卡住。微软建议加 `-UseBasicParsing`，或者如上设 `$PSDefaultParameterValues`。PowerShell 7 不受影响。https://support.microsoft.com/en-us/servicing/os/windows/2025/12/powershell-5-1-invoke-webrequest-preventing-script-execution-from-web-content

### 7.2 `Invoke-WebRequest` / `irm` 下载的文件有没有 Mark-of-the-Web

- **PowerShell 7**：搜遍了 PowerShell/PowerShell 仓库，`Zone.Identifier` 只出现在**读取**的地方（`ClrFacade.cs` 判断执行策略区域）和**删除**的地方（`Unblock-File`），Web cmdlet 里没有任何写入逻辑。所以 `Invoke-WebRequest -OutFile` 写出的文件**没有** MotW。**[已核实（源码）]** https://github.com/PowerShell/PowerShell/blob/master/src/Microsoft.PowerShell.Commands.Utility/commands/utility/UnblockFile.cs
- **Windows PowerShell 5.1**（闭源）：社区多次反映用 `Invoke-WebRequest` 或 `WebClient.DownloadFile` 下载的文件不带 Zone.Identifier（PowerShell.org 论坛 2021 年的帖子，无人反驳）。**[待实测]**：在干净的 Win10 和 Win11 上下载后运行 `Get-Item file.zip -Stream *`，看是否只有 `:$DATA`。https://forums.powershell.org/t/enfore-execution-policy-on-scripts-downloaded-using-invoke-webrequest/17892
- `irm` 是 `Invoke-RestMethod`，只返回内容不写文件（除非加 `-OutFile`），同样不加 MotW。`irm https://… | iex` 运行的脚本根本不落盘，也不受执行策略约束，所以安装命令本身要在官网用 HTTPS 发布，并在页面上公布 SHA256。
- 结论：脚本里下载的 zip 没有 MotW，`Expand-Archive` 解出的 exe 也就不会继承 MotW。反过来，**用户在浏览器里手动下载 zip** 时会带上 MotW，Windows 自带的解压会把 MotW 传给解出的文件。那条路径下双击 exe 会触发 SmartScreen。

### 7.3 SmartScreen / Smart App Control / Defender

- **SmartScreen** 针对的是「从互联网下载的应用」，按文件和签名的信誉判断（"checking downloaded programs and the digital signature"）。它的触发依赖 MotW，并由 Shell 启动程序时检查；SCM 启动服务不经过这一步。所以只要文件没有 MotW、由服务控制管理器启动，**服务本身不会弹 SmartScreen**。GUI 是由 Explorer 或 Run 键启动的，同样因为没有 MotW 而不检查。https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/
- **Smart App Control（Windows 11，干净安装时启用，先评估、后强制）** **[已核实]**：在强制模式下，"Malware, Potentially Unwanted Apps (PUA), and unknown, unsigned code are blocked by default"。它在执行时由代码完整性机制检查，**与 MotW 无关**，应用和二进制文件都会被检查。也就是说，**未签名的 exe 在开着 SAC 的 Win11 上可能根本跑不起来**（对服务是否同样拦截 **[待实测]**，但文档说的是 "apps and binaries"）。wintun.dll 本身有签名，不受影响。https://learn.microsoft.com/en-us/windows/apps/develop/smart-app-control/overview
- **Defender 误报**：代理 / TUN 类工具经常被机器学习启发式误报，例如 v2rayN 多次被报为 `Trojan:Win32/Wacatac.*!ml`（issue #1296、#1823）。未签名、新出现、内嵌网络栈、以 SYSTEM 运行，这些特征叠在一起都会加重嫌疑。缓解办法：代码签名；每个版本发布前用 https://www.microsoft.com/wdsi/filesubmission 提交为软件开发者样本；不要用 UPX 之类的加壳；符号可以 strip，但保留版本资源（`VERSIONINFO`）。https://github.com/2dust/v2rayN/issues/1296
- **签名**：Azure Artifact Signing（原 Trusted Signing）约 9.99 美元/月。个人开发者**只限美国和加拿大**；组织可申请的国家名单各来源说法不一致，需要到微软官方 FAQ 确认（可能影响我们）。传统的 OV / EV 证书是另一条路。https://learn.microsoft.com/azure/artifact-signing/quickstart
- **建议**：Windows 版在 1.0 之前就解决签名问题。否则 SAC 和 Defender 会让「出门插线就能用」的体验在相当一部分 Win11 机器上直接失败。

---

## 8. 构建（x64 + ARM64，Rust + C）

- **Rust 目标**：自 1.91（2025-10-30）起 `aarch64-pc-windows-msvc` 升为 **Tier 1**；`x86_64-pc-windows-msvc` 一直是 Tier 1。`*-pc-windows-gnullvm` 两个目标是 Tier 2（带宿主工具），需要 llvm-mingw 或 MSYS2 的 CLANG* 环境。https://blog.rust-lang.org/2025/10/30/Rust-1.91.0/ 、https://doc.rust-lang.org/rustc/platform-support/windows-gnullvm.html
- **选 MSVC 而不是 gnullvm**：windows-rs、windows-service、tun2proxy 发布物、wintun 头文件都以 MSVC 为主；如果日后 hev 走 clang-cl 路线，也是 MSVC ABI。gnullvm 只有在想从 Linux 或 macOS 完全不用微软 SDK 交叉编译时才有优势。
- **GitHub Actions** **[已核实]**：`windows-latest` / `windows-2025` / `windows-2022` 为 x64；**`windows-11-arm`**、`windows-11-vs2026-arm` 为 ARM64。私有仓库是 2 核 8 GB，按分钟计费，用完免费额度后收费。本仓库是 **PRIVATE**，而 Linux 设计文档提到「GitHub Actions 账单未处理」，所以 CI 需要先解决计费问题。https://docs.github.com/en/actions/reference/runners/github-hosted-runners
- **在 x64 上交叉编译 ARM64**：安装 VS 2022 / 2026 Build Tools 的 "MSVC ARM64 build tools" 组件后，`rustup target add aarch64-pc-windows-msvc && cargo build --target aarch64-pc-windows-msvc` 即可。`cc` crate 会通过 vswhere 找到 ARM64 的 `cl.exe`。ARM64 测试仍然要在真机或 `windows-11-arm` 上跑（x64 机器不能跑 ARM64 程序，反过来 ARM64 可以通过 Prism 仿真跑 x64）。
- **在 macOS / Linux 上交叉编译**：可以用 `cargo-xwin`（下载 MSVC CRT 和 Windows SDK，需要接受微软许可）或 gnullvm + llvm-mingw。适合本地快速检查能否编译，不建议用来出正式包。
- **C 部分**：
  - 若采用 Rust 协议栈，Windows 上除了 `wintun.dll`（预编译）没有 C 代码要编译。`hev-sys/build.rs` 在非 Linux 平台本来就直接返回。
  - 若日后采用 hev 的 MSVC / clang-cl 版本：`build.rs` 里给 Windows 加分支，调用 `cc::Build` 并设 `compiler("clang-cl")`，或者调用上游的 `build-msvc.cmd`。ARM64 依赖 hev-task-system PR #17 里新增的 ARM64 上下文切换汇编和 unwind 元数据。

---

## 9. 推荐架构

```
cellglide-service.exe（LocalSystem，SCM，自动启动，失败后重启）
 ├─ cg-core（原样复用）：usbmux → 127.0.0.1:27015（AMDS）、转发器 127.0.0.1:N、DoH、健康检查、状态机
 ├─ cg-platform/windows（新增）
 │   ├─ tun.rs：动态加载 wintun.dll；WintunCreateAdapter("cellglide0", "CellGlide", 固定 GUID)；进程退出即消失
 │   ├─ engine：tun2proxy 或 ipstack（第一阶段）→ SOCKS5 127.0.0.1:N；hev clang-cl 版（第二阶段，等上游合并）
 │   ├─ ip.rs：IP Helper，在我们的 LUID 上设 198.18.0.1/32、fd6c:6779:6465::1/128、四条 /1 路由、MTU 1500、跃点 0、DAD 0、关闭 RA
 │   ├─ dns.rs：SetInterfaceDnsSettings（只针对我们的 GUID）→ 198.18.0.1；解析器监听 198.18.0.1:53（UDP/TCP），失败时快速回 SERVFAIL
 │   ├─ strict_dns.rs（可选）：WFP 动态会话，拦截 53 端口，只放行我们的解析器
 │   └─ power.rs：SERVICE_CONTROL_POWEREVENT 加时钟跳变检测
 ├─ IPC：\\.\pipe\CellGlide\control，SDDL 为 SY/BA 全权、IU 读写；按令牌授权
cellglide-gui.exe（用户进程，iced + tray-icon，HKCU Run 自启）
cellglide.exe（CLI）
安装：install.ps1（下载 → 校验 SHA256 → 复制到 Program Files → New-Service → 开始菜单）；代码签名
```

## 10. 主要风险（按严重程度）

1. **ARM64 上 Apple 的 USB 驱动和 AMDS 能否用**：整个产品依赖 Apple Devices / iTunes 提供的 AMDS（127.0.0.1:27015）和 Apple 的 USB 内核驱动；而内核驱动在 ARM64 上必须是原生 ARM64 版本。Apple 的 Windows 应用已经能在 Copilot+ PC 上运行，但社区仍有「iPhone 识别不到」「USB 网络共享不可用」的报告（2026-07 Apple 社区、微软问答）。我们只需要 usbmux，不需要网络共享用的以太网驱动，但**必须在 Snapdragon 真机上验证**。不行的话，ARM64 版就没有意义。https://learn.microsoft.com/en-us/answers/questions/3863362/apple-usb-tethering-is-not-working-on-my-windows11 、https://allthings.how/apple-expands-windows-app-support-to-arm-based-copilot-pcs/
2. **转发引擎与 Mac / Linux 不一致**：hev 暂时无法在 Windows 上原生使用，Rust 协议栈在 TCP 行为、UDP 超时、性能上会和 hev 有差异，AGENTS.md 要求的「hev 设置一致」在 Windows 上需要换一种说法或单独豁免。
3. **未签名二进制**：Smart App Control 强制模式下会被直接拦截，Defender 也可能误报，而 Azure Artifact Signing 对个人和部分国家不开放。
4. **DNS 泄漏与 53 端口**：A 方案在超时或 SMHNR 并行时仍可能泄漏，需要实测；`198.18.0.1:53` 可能和 ICS / Hyper-V 冲突；不用 NRPT 意味着防泄漏强度不如 Tailscale。
5. **Modern Standby 收不到睡眠通知**：健康判定可能把睡眠误判为中断。
6. **CI 成本**：私有仓库的 ARM64 运行器要计费，而 Actions 账单尚未处理，ARM64 验证暂时只能依赖真机。
