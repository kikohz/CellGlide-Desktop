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
| ARM64 CI 编过并通过带 `wireless` 的测试 | **未通过**：OpenSSL 编过（10 分 57 秒），凡是做真实 TLS 握手的测试都以 `STATUS_ACCESS_VIOLATION` 崩溃，见「ARM64」一节 |
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

## 留下的东西

- `C:\ProgramData\CellGlide\wireless.json` 和 `wireless-keys.bin` 保留（1.0.0 的服务不读它们）；iPhone 的「已配对」列表里有这台电脑。
- Strawberry Perl 装在 `C:\Strawberry`（编 OpenSSL 用）。
- 计划任务 `CellGlideGate` 已删除。

## ARM64（未解决）

三轮 CI 诊断（运行 43509372741、43509856663、43512265357）查到的：

- 不做 TLS 握手的 48 个测试在 ARM64 上都过，包括 WLAN、WinRT 的；4 个做真实握手的测试全部崩溃。
- 崩溃点在 OpenSSL 里：`tls_parse_all_extensions+0x10`，读连接对象时指针值是 1。调用栈在**测试用的服务端**（`tokio_openssl` 的 `accept`，解析 ClientHello）。产品里只用客户端，客户端一侧是否也会崩溃还不知道。
- ARM64 上 OpenSSL 的构建：`VC-WIN64-ARM`、`no-asm`、MSVC 14.51（Visual Studio 18）`/O2`。
- 怀疑是 MSVC 在 ARM64 上把 OpenSSL 编错了，没有证实。

## 待办

- ARM64 CI 的崩溃查明并修复后，把结果和两种架构的 CI 时间补到这里。
- CI 的 x64 运行器上 OpenSSL 是否用了汇编（nasm）还没确认；ARM64 不用 nasm。
