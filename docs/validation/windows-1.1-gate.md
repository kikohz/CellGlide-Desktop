# Windows 1.1 无线连接：闸门验证

日期：2026-11-13 · 设备：这台电脑（Windows 11 26300，Intel Wi‑Fi 6 AX201，连着 Wi‑Fi「MamaFresh」）+ iPhone 16e（CellGlide 2.6.0，VPN 模式关）
计划：`docs/superpowers/plans/2026-11-13-windows-wireless-a.md` 任务 1 第 6–8 步、任务 8 第 5–6 步。
工具：`cargo build -p cg-platform --release --features wireless --example wireless_gate --locked`（提交 0f80a02）。

## 结论

| 闸门条件 | 结果 |
|---|---|
| LocalSystem 身份下开接入点 | 通过 |
| LocalSystem 身份下在接入点上发现 iPhone | 通过（两种做法都能找到；系统接口不稳定，见下） |
| LocalSystem 身份下用 vendored OpenSSL 完成配对通道握手 | 通过 |
| x64 CI 编过并通过带 `wireless` 的测试 | 通过（运行 43505868400） |
| ARM64 CI 编过并通过带 `wireless` 的测试 | 通过（运行 43522575450，OpenSSL 改用 clang-cl 编，见「ARM64」一节） |
| 插线自动配对，配来的密钥能在无线上用 | 通过 |

## 接入点、发现、握手

命令：`wireless_gate CellGlide-GATE <密码> <秒> --mdns both`。iPhone 手动连上这个网络后回到 CellGlide 前台共享。

| | 普通用户（iloss） | LocalSystem（一次性计划任务，跑完已删） |
|---|---|---|
| 接入点启动 | 107 ms | 344 ms |
| iPhone 连入到拿到地址 | 约 1.2 s（192.168.137.62） | 约 0.05 s |
| 发现：系统接口（`DnsServiceBrowse`/`Resolve`） | 找到 | 找到，但 iPhone 连入后的 22 轮里只有 12 轮找到 |
| 发现：自己收发（UDP 5353，只查不答） | 找到 | 找到，连入后每一轮都找到 |
| 握手 `CG1-pair`，TLS 1.2 `ECDHE-PSK-CHACHA20-POLY1305` | 342 ms | 99 ms |
| 发 `CG1 PAIR` 后收到 | `C <承诺> 4961212D-…` | `C <承诺> 4961212D-…` |

- iPhone 连入前，两种发现做法都返回「没有」，不报错。
- 接入点关掉再开（同名同密码）后，iPhone **没有**自动重新连上，要在设置里再点一次。计划 B 的「断开后重连」要按这个事实设计。
- 结论：计划 B 以「自己收发」为主要做法。

## 网络快照

`scripts/network-snapshot.ps1`：开始前、两遍闸门之后、全部结束（含服务换回原版）之后各一次。两次对比结果都是 `identical`。全程电脑的 Wi‑Fi 保持连着 MamaFresh；结束后 Wi‑Fi Direct 虚拟网卡为 Disconnected，没有 192.168.137.x 地址。

## 插线自动配对

做法：把已安装的服务临时换成本分支的构建（提交 0f80a02 的工作区，`cellglide-service.exe` 9,643,520 字节），服务的环境变量设 `CELLGLIDE_WIRELESS=1`，重启服务。验证后已换回 1.0.0 的原文件并删除环境变量。

- iPhone 插线共享后，服务日志：`wireless: paired over the cable`。USB 会话照常为 Connected。
- `C:\ProgramData\CellGlide\wireless.json`：一条记录（名字 `iPhone`，方式 `cable`，不过期），并记住了这台 USB 设备对应的手机标识。文件里没有密钥。
- `wireless-keys.bin`：358 字节，DPAPI 机器范围，能用同样的附加熵解开。
- iPhone 上的询问弹窗原文：待补（用户口头确认点了允许）。

## 用配来的密钥走无线

命令：`wireless_gate CellGlide-GATE <密码> 590 --mdns raw --data <identity> <长期密钥> 8088`。

- 握手用的身份是配对记录里的 identity，密钥是 HKDF(长期密钥, "CG2 link psk")：245 ms 完成，套件同上。
- 发 `CG1 DATA 8088` 和 hello 请求后收到 `HTTP/1.1 200 OK`，JSON 里的能力列表含 `wireless-v2`。
- 这同时在真机上验证了任务 3 里用 Python 独立算出的 `link_key` 固定值。

## 40 分钟驻留（iPhone 会不会留在接入点上）

起因：用户发现 iPhone 经常自己切到别的已记住的 Wi‑Fi（接入点没有外网）。这次在 iPhone 上关掉 MamaFresh 的「自动加入」来模拟「附近没有认识的 Wi‑Fi」，拔掉数据线，CellGlide 前台共享，放在电脑旁不动。`wireless_gate … 2400 --mdns raw`，13:42–14:22。

- 接入点开起来 7 秒后 iPhone 自己连上（没有手动操作），之后 40 分钟里只有这一次连入，没有断开重连。
- 每 2 秒一轮的发现查询共 1200 轮，找到 833 轮（69%）。没找到的多数是孤立的一两轮；连续没找到最长 20 秒，10 秒以上的共 7 次，后 15 分钟比前面多。
- 前后网络快照 `identical`。
- 没测：这期间数据连接是否一直可用（这次只查发现，不传数据）；iPhone 的掉电和它弹过什么提示（待用户补）。

对计划 B 的含义：附近没有别的已记住网络时，iPhone 能留住；但**不能用单次发现查询的结果判断手机在不在**，会话期间要靠已建立连接的存活来判断，发现只用来拿地址和端口，并且要容忍几十秒查不到。附近有已记住的、能上网的 Wi‑Fi 时 iPhone 会被抢走，这一点没有解决。

## 留下的东西

- `C:\ProgramData\CellGlide\wireless.json` 和 `wireless-keys.bin` 保留（1.0.0 的服务不读它们）；iPhone 的「已配对」列表里有这台电脑。
- Strawberry Perl 装在 `C:\Strawberry`（编 OpenSSL 用）。
- 计划任务 `CellGlideGate` 已删除。

## ARM64（已解决，提交 0f4b4a8）

- 现象：ARM64 上凡是做真实 TLS 握手的测试都以 `STATUS_ACCESS_VIOLATION` 崩溃；不做握手的 48 个测试都过。
- 不只是测试的问题：用我们的客户端去连外部的 `openssl s_server`，在 ARM64 上同样崩溃，所以产品里的客户端路径也会崩。
- 崩溃点在 OpenSSL 里（`tls_parse_all_extensions` 入口，连接指针的值是 1）。同一份 OpenSSL 源码，用 MSVC 14.51（Visual Studio 18）`/O2` 编就崩；用 `/Od` 或 clang-cl 编，7 个握手测试全过。结论：MSVC 14.51 的 ARM64 编译器把 OpenSSL 编错了。本机用 MSVC 14.44 交叉编出的那处调用是正确的，所以很可能是 14.51 的退化，但没有在 ARM64 上运行验证过。
- 修复：ARM64 上用 clang-cl 编 vendored OpenSSL（`scripts/openssl-clang-cl.cmd`，由 `CC` 指定）。`crates/cg-platform/build.rs` 在 ARM64 目标上打开 `wireless` 而 `CC` 不是这个包装脚本时直接拒绝构建，避免不小心编出会崩溃的版本。
- 绿的运行 43522575450：`windows-rust (arm64)` 24 分 23 秒（其中 13 分 47 秒是因为 `CC` 变了重编 OpenSSL，之后有缓存），`windows-rust (x64)` 5 分 42 秒；两边带 `wireless` 的 54 个测试全过，LNK4099 警告为 0。
- 留给计划 B：发布脚本 `scripts/build-windows.ps1` 在 ARM64 上打开 `wireless` 时要设同样的两个环境变量（`CC`、`CC_aarch64_pc_windows_msvc`）。

## 待办

- CI 的 x64 运行器上 OpenSSL 是否用了汇编（nasm）还没确认；ARM64 不用 nasm。
