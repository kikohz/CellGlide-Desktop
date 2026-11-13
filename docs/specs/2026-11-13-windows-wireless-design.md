# Windows 无线连接设计（1.1）

日期：2026-11-13 · 状态：待评审 · 分支：`windows`
前置：[Windows 1.0 设计](2026-10-29-windows-1.0-design.md)；Mac 版无线的设计在 CellProxy 仓库 `Docs/Specs/2026-10-23-p2p-wireless-design.md`（下称「Mac 设计」）。

## 1. 意图

用户出门没带数据线、身边也没有路由器时，Windows 电脑仍能用上 iPhone 的蜂窝网络。Mac 版靠苹果私有的点对点无线（AWDL）做到这一点，Windows 没有 AWDL，也没有任何 iPhone 能对接的直连技术（见 §3）。所以 Windows 版换一种建立链路的办法：**电脑自己开一个临时接入点，iPhone 像连普通 Wi‑Fi 一样连上来**。链路以上的部分（配对、加密、端口转发）全部沿用 iPhone App 现有的无线协议，iPhone 端一行不改。

## 2. 目标、非目标与反目标

目标：

- 没插线时，在 CellGlide 里点「无线连接」，iPhone 连上电脑的接入点并打开 CellGlide 后，电脑自动连上，之后分流、DNS、严格 DNS、网络质量、测速和 USB 时一样工作。
- 只有配对过的电脑能连。配对沿用 Mac 的两种方式：插线自动配对、核对 6 位验证码。
- 断开后立刻恢复原网络并自动重连一段时间；插上线自动切回 USB。
- 接入点只在用户点了「无线连接」之后存在，停止、崩溃、进程被杀时都消失。

非目标：

- 同一个路由器 Wi‑Fi 下的连接（用户已定：不做）。实现上不主动支持，也不在界面里提。
- 电脑去连 iPhone 的个人热点（依赖运营商放行热点，用户已定不走这条路）。
- 自动无线连接（没插线就自己开接入点）。只由用户点按钮发起。
- 让 iPhone 自动加入接入点（要改 iPhone App，见 §13）。
- Linux。本设计只在 Windows 上编译。
- 经无线读电池温度、健康度、循环次数、型号详情：这些走 USB 专有的 lockdown 服务，无线下拿不到，界面隐藏。

反目标（出现即算失败）：

- 动了用户正在用的 Wi‑Fi 连接、系统「移动热点」、Internet 连接共享，或其他网卡的任何设置。
- 没配对的设备能经接入点用到手机网络，或看到流量内容。
- 接入点在会话结束后还留着。

## 3. 为什么是「电脑开接入点」

| 方式 | 结论 |
|---|---|
| AWDL（Mac 用的） | 苹果私有，Windows 没有 |
| Wi‑Fi Direct | Windows 有，iPhone 不对 App 开放 |
| Wi‑Fi Aware | iOS 26 起对 App 开放，Windows 不支持 |
| 蓝牙 | 只有几百 Kbps，还要改 iPhone App |
| 电脑连 iPhone 个人热点 | 受运营商限制，用户不要 |
| **电脑开接入点（Wi‑Fi Direct 组 + 兼容接入点）** | **采用** |

2026-11-13 在这台电脑（Intel AX201）和 iPhone 16e（CellGlide 2.6.0，上架版协议）上的验证：

- `WiFiDirectAdvertisementPublisher` 开自主组（`IsAutonomousGroupOwnerEnabled`）加 `LegacySettings`，电脑原来的 Wi‑Fi 不断，多出一块「Microsoft Wi‑Fi Direct Virtual Adapter」，地址 `192.168.137.1/24`，Windows 自己给 iPhone 发地址（`192.168.137.205`）。
- `ListenStateDiscoverability` 为 `None` 时 iPhone 看不到网络，改成 `Normal` 后看到并连上。
- 在这块网卡上发 mDNS 查询能找到 `_cellglide-p2p._tcp`；TLS 1.2 `ECDHE-PSK-CHACHA20-POLY1305` 握手成功（OpenSSL 3.5）；发 `CG1 PAIR` 收到 iPhone 的 `C` 行；未知身份得到 alert 115（unknown_psk_identity）。
- iPhone 连着这个没有外网的 Wi‑Fi 时，App 的出口仍是蜂窝（iPhone 端强制 `requiredInterfaceType = .cellular`）。
- 电脑到 iPhone 的 ping：平均 67 ms（17–114），10 个包 0 丢失。
- 正常停止和强杀进程后，接入点都在几秒内消失，虚拟网卡回到 Disconnected。
- 网络快照前后对比：只有那块 Wi‑Fi Direct 虚拟网卡自己的 Tcpip 键有变化（`EnableDHCP 1→0`，多了 `Dhcpv6Iaid`、`Dhcpv6State`），是 Windows 写的；其他网卡、DNS、路由、NRPT 不变。
- iPhone 开着 VPN 模式时不开无线监听（开发版才有 VPN 模式）。

没验证的：以 LocalSystem 身份（服务里）开接入点；经无线跑真实流量的速度和长时间稳定性；其他型号的网卡。前两项是里程碑 1 的闸门（§12）。

## 4. 用户体验

### 4.1 设置

- 「无线连接」开关，**默认关**，说明：「没带线时，让 iPhone 经这台电脑开的临时 Wi‑Fi 连过来。」存在服务的设置里（`wireless_enabled`）。
- 关着时：不开接入点、面板没有无线按钮、USB 会话不做插线配对。无线会话中关掉它，会话经现有停止流程结束。
- 开关下面是「已配对的 iPhone」列表，可以移除；只在开关打开且有配对时显示。
- 打开开关时检查一次这台电脑能不能开接入点（§6.1）；不能的话开关回到关，并说明原因。

### 4.2 连接

没插线、开关打开时，面板和概览页显示次要按钮「无线连接」。点击后：

1. 服务开接入点。界面显示三步和网络名、密码（密码旁有复制按钮）：
   - 「在 iPhone 的“设置 → 无线局域网”里连接 **CellGlide‑XXXX**，密码 **xxxx‑xxxx‑xxxx**。」
   - 「回到 iPhone 上的 CellGlide，点“开始共享”，保持在前台。」
   - 「这台电脑会自动连上。」
   并注明：「iPhone 可能提示这个网络无法连接互联网，这是正常的，上网走的是蜂窝。」
2. iPhone 连上并在共享后，电脑发现它：
   - 已配对：直接连接，不弹确认。
   - 没配对：电脑显示「在 iPhone 上确认验证码 384 271」和「一致」「取消」；iPhone 弹窗显示同一个数字。两边都确认后配对完成并直接连上。
3. 连上后状态行写「已通过无线连接 · 建议插线更稳」。

网络名和密码每台电脑生成一次并一直沿用，iPhone 记住后，第二次起用户只需：电脑上点「无线连接」，iPhone 上打开 CellGlide。

等待 iPhone 最多 5 分钟；超时关掉接入点，显示「没有等到 iPhone」，按钮恢复。用户随时可以点「取消」。

### 4.3 断开与切换

- 断开（手机走远、锁屏、停止共享）：立刻恢复原网络（走现有停止流程撤掉隧道），接入点保留，每 2 秒重试、最多 60 秒，期间显示「正在重新连接…把手机放近一点，或插上数据线」；成功就重新建隧道；60 秒后放弃，关接入点，显示「无线连接失败」。
- iPhone 不再认这台电脑（握手得到 unknown_psk_identity）：立即结束、不重试，显示「这台 iPhone 已不再允许这台电脑无线连接，请重新配对」。这条配对标为「过期」，记录和密钥保留；下次无线连接时对这台 iPhone（按 mDNS 实例名匹配）重新走验证码配对，新配对保存后才替换旧的。
  例外：验证码配对刚完成后的 10 秒内，这个拒绝要重试而不是结束（iPhone 重新加载密钥之前会拒绝新身份，Mac 也是这样处理的）。
- 插上线：结束无线会话，关接入点，开始 USB 会话。无线会话是用户主动发起的，所以即使「自动连接」关着，这次也直接转到 USB；用户点过「断开」之后插线，则按现有的自动连接设置办。USB 永远优先，同一时间只有一个会话。
- 无线的 2 秒 / 60 秒重连是无线会话自己的规则，不走 USB 的退避（`controller.rs` 的 `wants()`）；两者在 `wireless::policy` 里合成一个判定，带测试。
- 用户点「断开」：停止会话，关接入点。

### 4.4 提示

- 连接质量差（判定见 §9）：「把手机放近一点，或插上数据线」。
- 等待阶段发现不了 iPhone 超过 60 秒、而接入点上已有设备连入：提示「请在 iPhone 上打开 CellGlide 并开始共享；如果开着 VPN 模式，请先关闭」。
- 系统「移动热点」开着：不开接入点，提示「请先关闭 Windows 的移动热点」。两者不能同时用，且系统热点优先。

### 4.5 CLI

`cellglide connect --wireless`（打印网络名、密码和步骤；验证码配对时在终端里问「一致？」）、`cellglide wireless list`、`cellglide wireless forget <名字或编号>`。`status` 在无线会话时标明「无线」。

## 5. 结构

全部网络动作在服务（LocalSystem）里，界面只经 IPC 显示和确认。

```
cellglide-service.exe
  cg-core::wireless            协议：请求行、配对、密钥派生、存储接口（平台无关，全平台测试）
  cg-core::controller          会话改为以「手机链路」为中心：Usb | Wireless
  cg-platform::windows::
    access_point.rs            Wi‑Fi Direct 组 + 兼容接入点（WinRT）
    mdns.rs                    在接入点网卡上找 _cellglide-p2p._tcp
    wireless_tls.rs            TLS 1.2 ECDHE‑PSK（OpenSSL）
    secrets.rs                 DPAPI 存取
```

| 位置 | 改动 |
|---|---|
| `crates/cg-core/src/wireless/`（新） | `request.rs`（`CG1 DATA <port>` / `CG1 PAIR`、端口白名单）、`pairing.rs`（X25519、承诺、验证码、长期密钥）、`code_message.rs`（C/M/P/OK/F 行、记录哈希、证明）、`cable.rs`（8092 端口的一问一答）、`store.rs`（配对记录、过期标记、USB 设备到手机的对应；密钥经 `SecretStore` trait）、`policy.rs`（开关、重连、切换的判定，纯函数） |
| `crates/cg-core/src/phone.rs`、`controller.rs` | 所有到手机的连接今天都经 `Phone::open`，在这里加链路种类：USB（现状）和无线（TLS 连接 + `CG1 DATA <port>` 行）。控制器里按序列号标识手机的地方（`Command::Connect`、`chosen_serial`、`Active.device`、快照、历史）改成一个手机标识：USB 仍是序列号，无线是 `wireless:<phoneID>`（与 Mac 相同）。无线会话不启动电池监视和 USB 速度检测。先做抽象、USB 行为不变、测试全过，再加无线 |
| `crates/cg-core/src/wireless/ports.rs`（新） | cg-platform 依赖 cg-core，所以平台能力经 trait 注入控制器（`ControllerConfig`）：`AccessPoint`（开、关、连入设备的地址）、`PhoneBrowser`（接入点上的手机列表）、`WirelessDialer`（按身份和密钥拨一条 TLS 连接）、`SecretStore`。测试用假实现 |
| `crates/cg-core/src/settings.rs` | `wireless_enabled`、接入点的网络名和密码（密码经 `SecretStore`） |
| `crates/cg-platform/src/windows/` | 上面四个新文件 |
| `crates/cg-ipc` | 无线状态（等待 iPhone、待确认验证码、已连接、重连中、失败原因）、动作（开始、取消、确认验证码、移除配对）、接入点的网络名和密码；`PROTOCOL_VERSION` 加 1 |
| `crates/cg-service` | 接线；doctor 增加「这台电脑能否开接入点」 |
| `crates/cg-ffi`、`windows/CellGlide.App` | 设置开关与已配对列表、无线按钮与步骤卡片、验证码确认对话框、无线状态文案 |
| `crates/cg-cli` | §4.5 |
| `crates/cg-i18n` | 三语文案 |
| `scripts/network-snapshot.ps1` | 拍快照时按网卡描述（`Microsoft Wi-Fi Direct Virtual Adapter*`）找出这些虚拟网卡，把它们的 GUID 和别名记进快照文件；`-Compare` 把两份文件里记下的这些网卡的全部条目（Tcpip/Tcpip6 键、DNS、接口参数、路由）排除在外，和现在排除 cellglide0 的方式一样。其余照旧，脚本测试补上这种情况 |
| `packaging/windows/uninstall.ps1` | 普通卸载（不带 `-Purge`）也删除 `wireless.json` 和 `wireless-keys.bin` |
| `AGENTS.md` | §11 |

## 6. 接入点

### 6.1 做法

- `WiFiDirectAdvertisementPublisher`：`IsAutonomousGroupOwnerEnabled = true`，`ListenStateDiscoverability = Normal`，`LegacySettings.IsEnabled = true`，设置网络名和密码；`WiFiDirectConnectionListener` 收到请求后 `WiFiDirectDevice.FromIdAsync` 接受，并从 `GetConnectionEndpointPairs` 取得本端地址（接入点网卡）和对端地址。
- 网络名 `CellGlide-` 加 4 位随机大写字母数字；密码 12 位随机小写字母数字，按 4‑4‑4 用连字符分开显示和设置。第一次打开开关时生成，之后不变；设置里提供「重新生成」（iPhone 上要重新输密码）。
- 能不能开：`Start()` 后状态为 `Started` 算能。`Aborted` 时：先用只读的 `WlanEnumInterfaces` 看有没有无线网卡（没有 →「这台电脑没有无线网卡」）；错误是 `RadioNotAvailable` →「无线局域网已关闭」；`ResourceInUse` → 只读地查一下系统移动热点的状态，开着就提示先关闭，否则「无线网卡正被其他功能占用」；其余 →「这台电脑的无线网卡不支持」。
- 生命周期：只在「等待 iPhone」和无线会话（含 60 秒重连）期间存在。`Stop()` 关闭；服务进程退出时随进程消失（已验证）。服务启动时不需要清理。
- 接入点上可以连入多台设备，但同一时间只和一台 iPhone 建会话。选哪一台：有已配对的就选它（多台已配对时选最近连过的）；没有已配对的、只有一台在广播 CellGlide 服务，就对它走验证码配对；有多台未配对的，不猜，提示「接入点上有多台 iPhone，请只留一台」。
- 连入的其他设备拿不到任何服务：服务不在这块网卡上提供任何东西，Windows 也没有为它开共享。

### 6.2 不做的事

- 不开、不关系统移动热点（`NetworkOperatorTetheringManager` 只用来读状态），不开 Internet 连接共享，不给接入点提供上网。
- 不断开、不切换电脑当前的 Wi‑Fi，不改信道。接入点的信道由 Windows 决定（电脑连着 Wi‑Fi 时跟它同信道）。
- 不在 Wi‑Fi Direct 虚拟网卡上写地址、DNS、路由、跃点。它的地址和 DHCP 由 Windows 管。

### 6.3 路由与严格 DNS

- iPhone 的地址在接入点网卡的 on‑link 网段里，前缀比隧道的 /1 路由长，到 iPhone 的连接自然留在这块网卡上，不进隧道，不需要加路由（与 1.0 设计 §5.4 同理）。
- 严格 DNS：这块网卡是 Wi‑Fi 类型（71）的非硬件网卡，按现有规则得到放行。没有任何 DNS 查询会走它（系统 DNS 仍只有 cellglide0 的），实现时用一条测试确认规则对它的归类，并在真机上跑 DNS 泄漏检测。

## 7. 发现 iPhone

- 用 Win32 的 `DnsServiceBrowse` / `DnsServiceResolve`（Windows 10 1809 起自带，不装 Bonjour），限定在接入点网卡的接口索引上，服务类型 `_cellglide-p2p._tcp`。
- 实例名就是手机的标识（phoneID）。解析出的地址必须落在接入点网段内，且等于 6.1 里接受的对端地址之一，否则忽略。
- 等待阶段持续浏览；会话期间保持浏览，用来判断手机是否还在。
- 如果系统 API 在服务身份或这块网卡上不可用（里程碑 1 验证），退回自己在接入点网卡地址上收发 mDNS（绑 UDP 5353 只为收应答，只发 PTR/SRV/A 查询，不应答别人的查询，所以仍然不算对外提供服务）；验证脚本已证明这种做法可行。

## 8. 加密链路与配对

协议与 Mac 设计完全相同，这里只列 Windows 端要实现的固定值。测试向量从 CellProxy 的 Swift 测试里取，保证两端算出同样的结果。

### 8.1 TLS

- 只用 TLS 1.2，只开 `TLS_ECDHE_PSK_WITH_CHACHA20_POLY1305_SHA256`，关会话恢复和 session ticket，每条连接完整握手。TCP keepalive：空闲 30 秒、间隔 10 秒、3 次。
- 配对通道：身份 `CG1-pair`，密钥 SHA256("CG1 public pairing key")，只能发 `CG1 PAIR`。
- 数据连接：身份是配对记录里的 identity，密钥是 HKDF‑SHA256(长期密钥, info "CG2 link psk", 32 字节)。握手后第一行 `CG1 DATA <port>\n`，端口只能是 9876、8088、8089、8090、8091。
- 实现：`openssl` crate 开 `vendored`，静态链进 `cellglide-service.exe`，只在 Windows 目标上依赖。SChannel 和 rustls 都不支持 PSK 套件。OpenSSL 是 Apache‑2.0，许可清单照常由 cargo‑about 生成。
- 握手失败且对端 alert 为 115：按「iPhone 不再认这台电脑」处理（§4.3，含配对后 10 秒的例外）。
- iPhone 给「握手 + 请求行」一共 5 秒，所以握手完成后立刻发请求行。
- 关闭语义（与 Mac 一致）：电脑这边的使用者只关了写的一半时，不结束这条连接、不发 `close_notify`，继续把 iPhone 的回复读完；iPhone 会在一方结束 5 秒后自己关掉转发。使用者整个关闭时直接断开 TCP。实现时对「请求发完后半关闭、回复随后到达」写一条测试。

### 8.2 验证码配对

行格式（二进制字段是无填充 base64url，名字是 UTF‑8 的 base64url，每行含换行不超过 512 字节）：

1. iPhone → `C <承诺> <phoneID>`。phoneID 必须等于连上的 mDNS 实例名，否则中止。
2. 电脑 → `M <公钥> <随机数16字节> <本机标识> <本机名字>`
3. iPhone → `P <公钥> <随机数>`。电脑核对 SHA256(公钥 ‖ 随机数) 等于承诺。
4. 验证码：SHA256("CG1 code" ‖ 手机公钥 ‖ 电脑公钥 ‖ 手机随机数 ‖ 电脑随机数) 前 4 字节按大端取整，模 1 000 000，补零到 6 位。显示前先做一次 X25519，低阶公钥直接中止。
5. 长期密钥：HKDF‑SHA256(X25519 共享秘密, salt = 手机随机数 ‖ 电脑随机数, info "CG1 long-term key", 32 字节)。
6. iPhone → `OK <identity> <手机名字> <证明>` 或 `NO`。证明 = HMAC‑SHA256(证明密钥, "CG1 phone ok" ‖ 记录哈希)；证明密钥 = HKDF‑SHA256(长期密钥, info "CG2 pairing proof")；记录哈希是 C、M、P 三行和 `OK <identity> <手机名字>\n` 按收发原样（含换行）的累计 SHA256。
7. 电脑核对证明（不符即中止）；**电脑用户也点了「一致」之后**先保存配对，再发 `F <HMAC(证明密钥, "CG1 mac ok" ‖ 记录哈希)>`。只有 iPhone 的用户确认、电脑用户没确认时不保存。先保存再发 F 是协议规定的顺序（iPhone 收到 F 才保存）：F 丢了的话电脑留下一条 iPhone 不认的记录，下次连接被拒后标为过期，再走一次验证码配对替换它。
8. 时限：电脑每步等对方 60 秒；从显示验证码起给两位用户共 75 秒。iPhone 只等 `M` 15 秒，所以收到 `C` 后立刻发 `M`。用户取消、超时、任何一步不符：什么都不保存。
9. iPhone 的限制（电脑端要配合，不要撞上）：连续 3 次配对失败后 60 秒内不接受配对；同时最多 2 条配对连接。电脑同一时间只开一条配对连接；被拒时显示「请稍后再试」，60 秒内不自动重试。
10. 字段限制：标识和 identity 是 1–64 个 `A–Z a–z 0–9 -` 字符；名字编码前截到 120 个 UTF‑8 字节（按字符边界），去掉控制字符。

名字显示前按 Mac 的 `displayName` 规则清理（去引号、控制和格式字符，折叠空白，最多 40 个字符）。本机名字用计算机名。

### 8.3 插线自动配对

- USB 会话中，开关打开、且 iPhone 的 hello 能力里有 `wireless-v2` 时，经 usbmux 连手机的 8092 端口问一次。iPhone 只在有电脑经 USB 连着共享时才理会新电脑，所以只在 USB 会话已经建立之后问；USB 会话进行中打开开关时立刻问一次，没有会话时等下一次会话。每个 USB 会话一次；已配对且没过期的跳过。
- 电脑 → `CG1 CABLE <本机标识> <本机名字>\n`（整行不超过 256 字节，后面不再发任何东西）；iPhone → `<phoneID> <identity> <base64 的 32 字节密钥> <手机名字>\n`（新配对，iPhone 上会问用户一次，电脑最多等 70 秒）或 `CG1 KNOWN <phoneID>\n`（已配对，清掉过期标记）。回复最长 512 字节。
- 失败不影响 USB 会话，只记日志。

### 8.4 存储

- 服务是 LocalSystem，配对属于这台电脑（不分 Windows 用户）。IPC 的授权规则不变：能连接、断开的用户，也能开始无线连接、确认验证码、移除配对。
- 配对记录（peerID、名字、identity、创建时间、方式、过期标记、USB 设备对应）和本机标识（随机 UUID）存服务数据目录下的 `wireless.json`；32 字节长期密钥和接入点密码用 DPAPI 机器范围加密后存 `wireless-keys.bin`，目录 ACL 沿用 1.0 的服务数据目录。
- 1.0 的普通卸载会保留 `C:\ProgramData\CellGlide\`（只有 `-Purge` 才删）。这两个文件里有密钥，所以普通卸载也删掉它们；重装后要重新配对。

## 9. 会话与健康判断

- 无线会话的开始、结束、切换全部走现有的 TUN、路由、DNS、严格 DNS 流程，隧道常量不变。
- 健康阈值用无线一档，数值与 Mac 相同：SOCKS 握手超时 2.5 秒，经隧道的小请求超时 5.5 秒，连续 3 次失败才算这条腿失败，最近 5 次握手中位数超过 1150 ms 算「弱」。真机数据出来后如果明显不合适，在验证文档里记下并调整。
- 连接历史记录这次是无线会话。lockdown 相关读取在无线时跳过，界面隐藏对应项。
- 电脑睡眠、唤醒：睡眠时结束无线会话并关接入点，唤醒后不自动重开。

## 10. 测试

自动化（全平台跑 `cg-core::wireless`）：

- 请求行：只放行白名单端口；带符号、前导零、空格、超长的都拒绝。
- 配对：与 Swift 测试向量一致的验证码和长期密钥；承诺不符失败；替换任一方公钥时两边验证码不同；记录哈希改动任一行时证明不过；低阶公钥中止；phoneID 与实例名不符中止；只有一边确认不保存。
- 行解析：非规范 base64url、字段数不对、超长行都拒绝。
- 策略：无线中插线切 USB；断开后 2 秒一次重试、60 秒放弃；unknown identity 立即结束并标过期；过期配对走验证码配对且按标识选手机；开关关掉结束会话。
- 存储：保存、读取、移除、过期标记；密钥不出现在 `wireless.json`。
- TLS：对一个本地 OpenSSL 服务端（同一套件、同样关掉恢复）握手成功；未知身份得到可识别的错误；协商结果不是指定套件时失败。
- 假手机（`testing::fake_phone`）加无线版，跑通「发现 → 配对 → 数据连接 → 断开重连」。

Windows 特权测试（CI 的托管运行器没有无线网卡，这部分只在真机跑）：开接入点、停止、强杀后消失；前后网络快照一致。

真机验收（iPhone 16e + 这台电脑，记进 `docs/validation/windows-1.1.md`）：

1. 插线共享完成自动配对；拔线后点「无线连接」，iPhone 连接入点并打开 App，不弹验证码直接连上；网页、视频、DNS 泄漏检测、测速正常。
2. 在 iPhone 上移除这台电脑：电脑立即结束会话并恢复原网络。再点「无线连接」：两边显示相同验证码，两边确认后连上；点「取消」则什么都不保存。
3. 没配对的设备连上接入点后，用 OpenSSL 直接做数据握手失败。
4. 手机拿远：提示放近 → 重连 → 60 秒后放弃，接入点关闭，原网络恢复。
5. 无线中插线：自动切到 USB，接入点关闭。
6. 无线 30 分钟：速度、延迟、iPhone 掉电，iPhone 是否自己离开了这个 Wi‑Fi。
7. 开始前、会话中、结束后各拍一次 `scripts/network-snapshot.ps1`，两两对比：除 cellglide0 和 Wi‑Fi Direct 虚拟网卡自己的条目外没有差异；电脑原来的 Wi‑Fi 全程不断。
8. 开着系统移动热点、关掉无线电、没有无线网卡（虚拟机）时的提示正确。

## 11. 对 AGENTS.md 的修改（需要用户同意）

新增一条：

> - On Windows, wireless sessions use one temporary access point: a Wi-Fi Direct group owner with legacy settings (`WiFiDirectAdvertisementPublisher`), started only when the user asks for a wireless connection, stopped when the session ends, and gone with the process on a crash. Never turn on Windows Mobile Hotspot or Internet Connection Sharing, never disconnect or change the Wi-Fi network the computer is on, and never write addresses, DNS, routes or metrics on the Wi-Fi Direct virtual adapter. The Wi-Fi Direct virtual adapter's own entries (what Windows itself sets on it) are left out of the snapshot comparison, like cellglide0's.

并在「This is the only system-wide network effect allowed」一句里把严格 DNS 和这个接入点并列为仅有的两项。

## 12. 里程碑

每个里程碑结束时 USB 行为不变、测试全过。

1. **闸门**：服务身份（LocalSystem）下开接入点、在它上面发现 iPhone、用 vendored OpenSSL 完成配对通道握手；x64 和 ARM64 的 CI 都能编出带 OpenSSL 的服务。任何一项不成立就停下来报告（备选：接入点和发现放到用户会话里的进程；TLS 换 mbedTLS）。
2. `cg-core::wireless` 协议与存储，带测试向量。
3. `PhoneLink` 抽象，USB 行为不变。
4. 插线自动配对。
5. 接入点、发现、无线会话、重连与切换（CLI 可用）。
6. 验证码配对。
7. 界面、文案、doctor。
8. 真机验收、文档、发布说明。

## 13. 以后（不在 1.1）

- iPhone App 用 `NEHotspotConfiguration` 自动加入接入点：电脑经 USB（插线配对时）把网络名和密码交给 iPhone，以后用户只需在 iPhone 上点一次同意。要改 CellProxy 仓库，另行设计。
- 拔线后自动转无线。

## 14. 已替用户定下的事（可以推翻）

- 版本号 1.1.0。
- 开关默认关。
- 等待 iPhone 5 分钟超时。
- 网络名和密码固定不变（方便 iPhone 记住），可手动重新生成。
- 配对属于整台电脑，不分 Windows 用户。
- TLS 用静态链接的 OpenSSL。
- CLI 命令的写法。
- 普通卸载也删除配对密钥。
- 无线会话中插线，即使「自动连接」关着也直接转到 USB。
- 接入点上有多台未配对的 iPhone 时不猜，让用户只留一台。

## 15. 风险

- **服务身份下开不了接入点。** 验证是在普通用户身份下做的。里程碑 1 先验证；不行就把接入点放到用户会话的进程里，由服务指挥。
- **网卡兼容性。** 只验证了 Intel AX201。不支持的电脑上功能不可用，界面要说清原因。发布说明里写明「取决于无线网卡」。
- **iPhone 对没有外网的 Wi‑Fi 的处理。** 可能提示无互联网，也可能过一段时间自己离开；30 分钟验收里观察。如果会离开，文案里提示用户在弹窗里选「继续使用」。
- **OpenSSL 的构建成本。** vendored 构建需要 Perl 和较长的首次编译；CI 有缓存，本机要装 Perl。ARM64 运行器上能否构建在里程碑 1 确认。
- **同信道共用。** 接入点和电脑的 Wi‑Fi 共用一块网卡，延迟和吞吐会受影响；验收时测量。
- **自己实现的配对协议。** 与 Mac 相同的协议，用同一套测试向量对齐；实现完成后重点审查这一段。
