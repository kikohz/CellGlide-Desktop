# Windows 无线连接 B：无线会话、配对界面、设置与验收 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 没插线时，用户点「无线连接」，电脑开临时接入点，iPhone 连上并打开 CellGlide 后，电脑经无线用上手机的蜂窝网络；带验证码配对、重连、插线切回 USB、设置和命令行。

**Architecture:** 控制器（`cg-core::controller`）多一种会话：手机链路不再只是 usbmux 设备，而是 `Link::Usb | Link::Wireless`，两者都给出 `open(port) -> MuxStream`，所以转发器、DoH、状态、指标照旧。接入点、发现、TLS 拨号是三个 trait（`cg-core::wireless::ports`），Windows 实现在 `cg-platform`（计划 A 已有底层模块），测试用内存里的假实现。界面只经 IPC 看状态、发动作。

**Tech Stack:** Rust（tokio、计划 A 的 `cg-core::wireless` 与 `cg-platform` 的 `wireless` feature）、cg-ipc JSON、cg-ffi、WinUI 3（C#）、PowerShell 脚本。

**Spec:** `docs/specs/2026-11-13-windows-wireless-design.md`。真机事实：`docs/validation/windows-1.1-gate.md`。计划 A：`docs/superpowers/plans/2026-11-13-windows-wireless-a.md`。

## Global Constraints

- iPhone 端不改；CellProxy 仓库只读。
- 只动 CellGlide 自己的东西。接入点只由 `WiFiDirectAdvertisementPublisher` 开关；不碰用户的 Wi‑Fi、移动热点（只读状态）、Internet 连接共享、其他网卡；不在 Wi‑Fi Direct 虚拟网卡上写任何设置。
- 接入点只在「等待 iPhone」和无线会话（含重连）期间存在；取消、失败、断开、睡眠、切到 USB、服务退出时都关掉。
- USB 行为不变：现有控制器测试一个不改地通过（确需改动的逐个在报告里说明）。每个任务结束：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo clippy -p cg-platform --features wireless --all-targets --locked -- -D warnings`、`cargo test --workspace --locked` 全过；动了 C# 的任务再过 `scripts/check-windows-core.ps1`。
- 验证码、密钥、接入点密码不进日志。接入点密码只经 IPC 给有完整权限的客户端。
- 文案只有两种语言：`crates/cg-i18n/locales/en.yml`、`zh-CN.yml`，键登记在 `crates/cg-i18n/src/lib.rs` 的 `KEYS`；键必须是字面量。（设计里写的「三语」不对，以代码为准。）
- 新的 IPC 类型要补 `crates/cg-ffi/src/api/samples.rs` 的样例并用 `CG_FFI_BLESS=1` 重写夹具，C# 的 `Models/*.cs` 同步。
- 子代理不得发送合成键鼠输入、不得改系统设置、不得运行会开接入点的程序、不得动已安装的服务和 `C:\ProgramData\CellGlide`。真机步骤由主持人和用户做。
- 提交用已配置的 noreply 身份；可推 `windows`；不推 main、不发布。
- 固定数值（设计 §4.3、§8、§9 与真机结果）：
  - 等待 iPhone 最多 300 秒。会话断开后每 2 秒重试、最多 60 秒。配对刚完成 10 秒内的 unknown identity 重试。
  - TLS 拨号（TCP 连接 + 握手 + 写出请求行）总时限 4 秒（iPhone 给 5 秒）。
  - 发现：每轮 2 秒的 Raw 查询；**一轮查不到不算手机离开**（真机上约三成的轮次查不到，最长连续 20 秒）。
  - 无线健康档：SOCKS 握手超时 2.5 秒；经隧道的小请求超时 5.5 秒；连续 3 次失败才算失败；最近 5 次握手中位数超过 1150 ms 算「弱」。
  - 无线手机的标识：`wireless:<phoneID>`。
  - `PROTOCOL_VERSION` 6 → 7。

## Review Focus

1. usbmuxd 不在（没装 Apple Devices、服务停了）时无线会话仍能开始和保持；状态不被报成 `NoUsbService`（今天 `Actor::state()` 先看 `mux_up`）。
2. 无线会话中 usbmuxd 掉线或另一台手机插拔，不得拆掉无线会话；只有「有手机经 USB 可用」才触发切换。
3. 任何结束路径之后接入点都已关闭：用户取消、300 秒超时、配对被拒、60 秒重连失败、睡眠、切 USB、关开关、服务关闭、任务被 abort。
4. 使用者只关写的一半时，TLS 连接不发 close_notify、不断开，回复能读完；整个关闭时直接断 TCP。
5. 验证码配对进行中，用户取消、手机离开、开关被关、插上线，都不留下保存了一半的配对，也不留下卡住的「等待确认」状态。

---

### Task 1: 手机链路抽象（USB 行为不变）

**Files:**
- Modify: `crates/cg-core/src/phone.rs`、`controller.rs`、`battery.rs`、`lockdown.rs`（只改取设备的地方）、`crates/cg-core/src/testing/*`
- Create: `crates/cg-core/src/wireless/ports.rs`

**Interfaces:**
- Produces:
  - `ports::WirelessLink`（trait，`Send + Sync`）：`fn phone_id(&self) -> &str`；`fn open(&self, port: u16) -> BoxFuture<'_, Result<MuxStream>>`。
  - `phone::Link::{Usb { endpoint: MuxEndpoint, device: Device }, Wireless(Arc<dyn WirelessLink>)}`；`Phone { pub link: Link }`；`Phone::usb(endpoint, device)`；`Phone::open(port)` 按链路分派；`Phone::id() -> String`（USB 是序列号，无线是 `wireless:<phoneID>`）；`Phone::usb_device() -> Option<&Device>`；`Phone::is_wireless()`。
  - `Active.device: Device` 改为 `Active.phone: Phone`。

- [ ] **Step 1:** 先写测试：`Phone::usb(..).id()` 等于序列号；一个假的 `WirelessLink` 的 `Phone::id()` 是 `wireless:ABC`，`open(8088)` 走到假链路；`usb_device()` 对无线为 `None`。
- [ ] **Step 2:** 实现并把代码摸底里列出的每一处 `phone.device` / `device.serial` / `mux_id` 用法改到新接口：只在 USB 上有意义的（`usb_speed::link_speed`、`battery::watch` 的 lockdown、`lockdown::describe`、插线配对、按 `mux_id` 的拔出判断）用 `usb_device()` 取，取不到就跳过。本任务不引入任何无线行为。
- [ ] **Step 3:** 现有测试全过，未改动任何现有测试。Commit。

### Task 2: 判定规则与无线健康档

**Files:**
- Create: `crates/cg-core/src/wireless/policy.rs`
- Modify: `crates/cg-core/src/controller.rs`（`HealthTiming`、`HealthWatchTiming`、`JudgeConfig` 加无线档的构造函数；`quick_failures` 的阈值 2 改成取自配置）、`crates/cg-core/src/health/*`

**Interfaces:**
- Produces:
  - `policy::WAIT_FOR_PHONE = 300 s`、`RETRY_EVERY = 2 s`、`RETRY_FOR = 60 s`、`FRESH_PAIRING_GRACE = 10 s`、`DIAL_TIMEOUT = 4 s`。
  - `policy::pick(found: &[FoundPhone], records: &[Record]) -> Pick`，`Pick::{None, Paired { phone: FoundPhone, record: Record }, Unpaired(FoundPhone), SeveralUnpaired}`：有已配对的选它（多台取 `created_at` 最新的；过期的记录按未配对处理但只针对那台手机）；否则恰好一台就是 `Unpaired`；多台未配对是 `SeveralUnpaired`。
  - `policy::after_dial_failure(error: &DialError, since_pairing: Option<Duration>) -> Next`，`Next::{Retry, EndStale, EndFailed}`：unknown identity 且配对不到 10 秒 → `Retry`；unknown identity → `EndStale`；其余 → `Retry`（由 60 秒窗口兜底）。
  - `HealthTiming::wireless()`、`HealthWatchTiming::wireless()`、`JudgeConfig::wireless()`（数值见 Global Constraints）；`health` 里新增「弱」：`weak(median_of_last_5_handshakes: Duration) -> bool`，阈值 1150 ms，只在无线档启用。

- [ ] **Step 1: 写失败的测试**：`pick` 的每个分支（含「两台已配对取最新」「过期记录的手机走未配对」「一台已配对加一台陌生的选已配对的」）；`after_dial_failure` 的三种结果；无线档的每个数值；5 次握手 `[200,300,1200,1300,1400]` 算弱、`[200,300,400,1300,1400]` 不算。
- [ ] **Step 2–3:** 实现，通过，Commit。

### Task 3: 假的接入点、发现、拨号与无线版假手机

**Files:**
- Modify: `crates/cg-core/src/wireless/ports.rs`、`crates/cg-core/src/testing/mod.rs`
- Create: `crates/cg-core/src/testing/fake_wireless.rs`

**Interfaces:**
- Produces（`ports.rs`）：
  - `AccessPoint` trait：`fn start(&self, ssid: &str, passphrase: &str) -> BoxFuture<Result<Box<dyn RunningAccessPoint>, ApFailure>>`；`ApFailure::{NoAdapter, RadioOff, MobileHotspotOn, InUse, Unsupported}`。
  - `RunningAccessPoint`（`Send`，Drop 即关闭）：`fn local_address(&self) -> Option<IpAddr>`；`fn clients(&self) -> Vec<IpAddr>`；`fn lost(&mut self) -> BoxFuture<'_, ()>`（接入点被系统中止时完成）。
  - `Browser` trait：`fn browse(&self, local: IpAddr, timeout: Duration) -> BoxFuture<io::Result<Vec<FoundPhone>>>`；`FoundPhone { instance: String, address: IpAddr, port: u16 }`（从 `cg-platform::mdns_parse` 搬到这里，`cg-platform` 改为引用它）。
  - `Dialer` trait：`fn dial(&self, to: SocketAddr, identity: &str, psk: &Key) -> BoxFuture<Result<MuxStream, DialError>>`；`DialError::{UnknownIdentity, Timeout, Io(String)}`。返回的流必须满足 Review Focus 4。
  - `struct WirelessPorts { access_point: Arc<dyn AccessPoint>, browser: Arc<dyn Browser>, dialer: Arc<dyn Dialer> }`；`ControllerConfig.wireless_ports: Option<WirelessPorts>`。
  - `ports::TlsLink`：`WirelessLink` 的实现，持有 `(SocketAddr, identity, link_key, Arc<dyn Dialer>)`，`open(port)` = `dial` 后写 `Request::Data(port).line()`，整个过程受 `DIAL_TIMEOUT` 限制。
- Produces（测试用）：`FakeWireless`：可脚本化的三件套（接入点启动失败、`lost`、手机在第几轮被发现、几台手机、拨号结果），拨号成功时把流接到 `FakePhone` 的对应端口（先读走并核对 `CG1 DATA <port>\n` 或 `CG1 PAIR\n`；`PAIR` 接到一个用 `code_message` 写的假手机配对端）。记录：接入点开关次数、当前是否开着、每次拨号的身份。

- [ ] **Step 1:** 测试：`TlsLink::open` 写出的第一行正确、超时受限、端口不在白名单时不拨号直接报错；`FakeWireless` 自身的记录功能。
- [ ] **Step 2–3:** 实现，通过，Commit。

### Task 4: 无线会话（已配对的手机）

**Files:**
- Modify: `crates/cg-core/src/controller.rs`、`crates/cg-ipc/src/lib.rs`（只加内部要用的状态类型，对外协议在任务 7 一起升版本）

**Interfaces:**
- Consumes: 任务 1–3、`WirelessStore`、`Stored.wireless_enabled`。
- Produces:
  - `Command::ConnectWireless`、`Command::CancelWireless`。
  - 控制器内部的无线阶段 `WirelessPhase::{Off, Starting, Waiting { since, ssid }, Dialing, Connected, Reconnecting { since }, Failed(WirelessFailure)}`；`WirelessFailure::{AccessPoint(ApFailure), NoPhone, SeveralPhones, PairingStale, Lost, TurnedOff}`。
  - 快照里新增 `wireless: Option<WirelessStatus>`（阶段、网络名、失败原因、是否「弱」；密码不在快照里）。

行为：
- `ConnectWireless` 只在开关打开、没有活动会话、没有 USB 手机可用时接受；否则回 `Reason::BadRequest`。它启动一个无线任务：取 `store.access_point()` → `access_point.start` → 循环 `browser.browse(local, 2 s)` → `policy::pick` → 已配对则建 `TlsLink`，调用**与 USB 相同的** `establish`（经任务 1 的 `Phone`），健康用无线档，不启动电池的 lockdown 读取、USB 速度、插线配对。未配对的情形在任务 5。
- `Actor::state()`、`tick`、mux 处理：无线阶段不是 `Off` 时，状态由无线阶段决定，不看 `mux_up`；usbmuxd 掉线不拆无线会话。
- 会话结束信号沿用现有的（watch 关闭、DNS 变了、完整检查失败、转发停止），另加：拨号得到 `EndStale` → 标过期、结束、不重连；`RunningAccessPoint::lost` → 结束（`Lost`）。
- 结束后（除 `EndStale`、用户断开、切 USB、睡眠、关开关外）进入 `Reconnecting`：**先走现有 teardown 撤掉隧道**，接入点保留，每 2 秒浏览并重拨，60 秒内成功就重新 `establish`，否则关接入点、`Failed(Lost)`。
- 300 秒没等到手机：关接入点、`Failed(NoPhone)`。
- USB：无线阶段不是 `Off` 时有 iPhone 经 USB 出现且可选 → 结束无线、关接入点、`manual = true`、按现有流程连 USB（即使 `auto_connect` 关着）。
- `Disconnect`、`CancelWireless`、`SetSettings` 关掉开关、`Sleep { sleeping: true }`、shutdown：结束无线、关接入点；睡醒不自动重开。
- 连接历史记录这次是无线（`HealthMonitor` 的会话标记加一个字段）。

- [ ] **Step 1: 写失败的控制器测试**（`FakeWireless` + 现有 `Rig`，时间用 `tokio::time::pause` 的地方单列）：
  - 已配对手机：`ConnectWireless` → Connected，快照的手机标识是 `wireless:<id>`，隧道已建，接入点开着；`Disconnect` 后接入点关了。
  - **usbmuxd 没起来**（`MUX_UP_AT_START = false` 的配置）时同样成功，状态从不出现 `NoUsbService`。
  - 无线会话中 `FakeMux::drop_listeners()`：会话仍 Connected。
  - 手机第 5 轮才被发现、之后有 3 轮查不到：会话不受影响。
  - 300 秒没发现：`Failed(NoPhone)`，接入点关了。
  - 假手机 `end_session()`：隧道先撤、进入 `Reconnecting`、2 秒后重拨成功回到 Connected，接入点全程只开了一次。
  - 重连 60 秒都失败：`Failed(Lost)`，接入点关了。
  - 拨号得到 unknown identity：立即 `Failed(PairingStale)`，记录被标过期且**没有被删**，没有重连。
  - 接入点启动失败的每一种 → 对应的 `Failed(AccessPoint(..))`。
  - `RunningAccessPoint::lost` 完成：会话结束，`Failed(Lost)`。
  - 无线中 `mux.attach(device)`：切到 USB 并 Connected（`auto_connect` 关着也切），接入点关了。
  - 无线中 `Sleep { sleeping: true }`：会话结束、接入点关了；`sleeping: false` 后不自动重开。
  - 开关关着时 `ConnectWireless` 被拒；无线中关掉开关：会话结束。
  - 已有 USB 会话时 `ConnectWireless` 被拒，USB 会话不受影响。
  - 无线会话没有打开 8092，也没有 lockdown 连接（假手机的计数为 0）。
- [ ] **Step 2–3:** 实现，通过（现有测试不改），Commit。

### Task 5: 验证码配对接入会话

**Files:**
- Modify: `crates/cg-core/src/controller.rs`

**Interfaces:**
- Produces: `WirelessPhase::Pairing { code: String, phone_name: Option<String> }`；`Command::ConfirmCode { matches: bool }`；`WirelessFailure::{PairingDenied, PairingFailed, PairingBusy}`。

行为：`pick` 得到 `Unpaired`（或过期记录对应的手机）→ 用 `PAIRING_IDENTITY` 和 `pairing_channel_key()` 拨号 → `code_pairing::run(stream, expected_phone_id = 实例名, local_id, 计算机名, confirm, save)`：`confirm` 把验证码放进阶段并等 `ConfirmCode`；`save` 写入 `WirelessStore`（方式 `Code`，替换同一手机的旧记录）。成功后直接按任务 4 连接，10 秒宽限期内的 unknown identity 重试。`Denied` → `PairingDenied`；其余错误 → `PairingFailed`；被 iPhone 的失败锁定挡住（配对通道连得上但立刻被关）→ `PairingBusy`，60 秒内不自动再试。`SeveralUnpaired` → `Failed(SeveralPhones)`，接入点保留到 300 秒窗口结束，手机数变成一台后继续。

- [ ] **Step 1: 写失败的测试**：
  - 未配对手机：阶段出现 `Pairing { code }`，验证码与假手机算出的一致；`ConfirmCode { matches: true }` 且假手机允许 → 保存了记录、随后 Connected。
  - `matches: false` → `PairingFailed`，没保存，接入点关了。
  - 假手机回 `NO` → `PairingDenied`，没保存。
  - 等待确认时 `CancelWireless` / `Disconnect` / 关开关 / 睡眠 / 插上线：都没保存，阶段回到 `Off`（或切到 USB），没有残留的 `Pairing`。
  - 75 秒没确认 → `PairingFailed`。
  - 配对后第一次数据拨号得到 unknown identity、第二次成功：会话建立，记录没被标过期。
  - 两台未配对手机 → `SeveralPhones`；其中一台消失后自动继续配对。
  - 过期记录：对**那台**手机走配对，成功后旧 identity 的密钥没了。
  - 没有 `Pairing` 阶段时 `ConfirmCode` 被拒（`BadRequest`），不改变任何状态。
- [ ] **Step 2–3:** 实现，通过，Commit。

### Task 6: Windows 上的三个实现并接进服务

**Files:**
- Create: `crates/cg-platform/src/windows/wireless_ports.rs`
- Modify: `crates/cg-platform/src/windows/{access_point,mdns,wireless_tls}.rs`、`crates/cg-platform/Cargo.toml`、`crates/cg-service/Cargo.toml`、`crates/cg-service/src/windows/mod.rs`、`crates/cg-service/build.rs`（没有就新建）、`scripts/update-licenses.sh`（如需）

**Interfaces:**
- Produces: `cg_platform::windows::wireless_ports::ports() -> WirelessPorts`。
  - 接入点：包住计划 A 的 `AccessPoint`；`lost()` 由发布者的 `StatusChanged` 变成 `Aborted` 触发（补上计划 A 留的缺口）；`ApFailure` 从 `ApError` 映射（`InUse { mobile_hotspot: true }` → `MobileHotspotOn`）。
  - 发现：`mdns::browse_with(Method::Raw, ..)` 放在 `spawn_blocking` 里。
  - 拨号：`wireless_tls::connect` 的超时改为参数；返回一个包装流 `HalfCloseSafe`：`poll_shutdown` 只记下「写已结束」并返回 `Ok`，不调用 `SSL_shutdown`、不关 TCP 写端；Drop 时把套接字设为 `SO_LINGER 0` 直接断开。
- `cg-service` 在 Windows 上打开 `cg-platform/wireless`，`config.wireless_ports = Some(ports())`；链接参数 `/ignore:4099` 加到 `cg-service` 的 build.rs；删除 `CELLGLIDE_WIRELESS` 环境变量和 `wireless_forced`（开关从任务 7 的 IPC 来）。
- 许可清单带 feature 重新生成（Rust 的 openssl 各 crate 会回到清单里）。

- [ ] **Step 1:** 测试（回环，`wireless` feature）：`HalfCloseSafe`：客户端写完请求后 `shutdown()`，本地 OpenSSL 服务端随后发的回复能完整读到，服务端没有收到 close_notify；Drop 后服务端读到连接重置或 EOF。`ApError` → `ApFailure` 的映射表。拨号超时参数生效。
- [ ] **Step 2:** 实现；`cargo build -p cg-service --release --locked` 通过，记下服务程序大小的变化。
- [ ] **Step 3:** **ARM64：** 本任务依赖计划 A 遗留的 ARM64 上 OpenSSL 握手崩溃得到解决（见 `docs/validation/windows-1.1-gate.md`「ARM64」）。如果到这一步仍未解决：ARM64 的服务构建**不打开** `wireless` feature，`wireless_ports = None`，界面按「这台电脑不支持」处理，并在报告里写明；不要在 ARM64 上发布会崩溃的握手代码。
- [ ] **Step 4:** Commit，推送，x64 和 ARM64 的 CI 都绿。

### Task 7: IPC

**Files:**
- Modify: `crates/cg-ipc/src/lib.rs`、`describe.rs`、`describe/ui.rs`、`crates/cg-service/src/access.rs`、`server.rs`、`crates/cg-core/src/controller.rs`（`Command::SetSettings` 加字段）、`crates/cg-i18n/*`

**Interfaces:**
- Produces:
  - `PROTOCOL_VERSION = 7`。
  - `Settings.wireless_enabled: bool`（`#[serde(default)]`）；`Request::SetSettings` 加 `wireless: Option<bool>`（`#[serde(default)]`）。
  - `Request::{ConnectWireless, CancelWireless, ConfirmCode { matches: bool }, WirelessInfo, ForgetPhone { peer_id: String }, RegenerateAccessPoint}`；全部 `needs_full`，包括 `WirelessInfo`（它返回密码）。
  - `Response::Wireless { info: WirelessInfo }`；`WirelessInfo { available: Option<bool>, ssid: String, passphrase: String, paired: Vec<PairedPhone> }`；`PairedPhone { peer_id, name, method, stale, created_at }`。
  - `Snapshot.wireless: Option<WirelessStatus>`：`WirelessStatus { phase: WirelessPhaseView, ssid: Option<String>, code: Option<String>, weak: bool }`；`WirelessPhaseView`（tag `phase`）：`Waiting, Pairing, Connecting, Connected, Reconnecting, Failed { reason: WirelessFailure }`。
  - `Snapshot.wireless_offered: bool`：开关打开、平台有实现、没有活动会话、没有 USB 手机时为真（界面据此显示按钮）。
  - `ConnState::Connected` 加 `wireless: bool`（`#[serde(default)]`）。
  - `describe`：`wireless_line(&Snapshot) -> Option<String>`、`wireless_steps(ssid, passphrase) -> [String; 3]`、`wireless_failure_text(&WirelessFailure) -> String`；键 `wireless.*`（文案取自设计 §4，中英各一份）。
- 打开开关时（`SetSettings { wireless: Some(true) }`）做一次能力探测：`access_point.start` 后立刻关；失败则开关保持关并回 `Response::Error`，原因是新的 `Reason::WirelessUnavailable { cause: ApFailure }`。

- [ ] **Step 1:** 测试：新旧 JSON 兼容（没有新字段的版本 6 快照能读）；每个新 `Request` 的权限；只读客户端拿不到 `WirelessInfo`；`Reason::all()`、`KEYS`、两种语言齐全（现有的完整性测试会强制）；`wireless_offered` 的每个条件；开开关时探测失败的路径；`ForgetPhone` 正在用的那台 → 先结束会话再删。
- [ ] **Step 2–3:** 实现，夹具用 `CG_FFI_BLESS=1` 重写，通过，Commit。

### Task 8: 命令行

**Files:** Modify `crates/cg-cli/src/main.rs`、`crates/cg-i18n/*`

- `cellglide connect --wireless`：发 `ConnectWireless`，取 `WirelessInfo` 打印三步说明（含网络名和密码），订阅快照：`Pairing` 时打印验证码并问「一致？(y/N)」（非终端时打印说明并当作否），按回答发 `ConfirmCode`；等到 `Connected` 或 `Failed`（最多 330 秒），Ctrl‑C 发 `CancelWireless`。
- `cellglide wireless list` / `forget <编号或名字>` / `regenerate`；`cellglide settings --wireless on|off`；`status` 在无线会话时多一行（`describe::wireless_line`）。
- [ ] 测试（现有的命令行测试方式）：参数解析；`forget` 名字有歧义时列出并拒绝；非终端的确认路径。实现，Commit。

### Task 9: 界面层（cg-ffi 视图）

**Files:** Modify `crates/cg-ffi/src/views/overview.rs`、`api.rs`、`api/samples.rs`、`runtime.rs`、夹具

- `OverviewView.wireless: Option<WirelessView>`：`offered`、`phase`、`title`、`detail`、`steps: Vec<String>`、`ssid`、`code`、`hint`、`can_cancel`。
- `SettingsView.wireless: WirelessSettingsView`：`enabled`、`available_text`、`paired: Vec<PairedPhoneView>`。
- DLL 命令：`WirelessStart`、`WirelessCancel`、`WirelessConfirm { matches }`、`WirelessForget { peer_id }`、`WirelessRegenerate`、`WirelessInfo`（回复 `ReplyValue::Wireless`，带密码，只在用户点开步骤卡片时取）。
- [ ] 样例和夹具齐全（编译会强制）；视图的测试：每个阶段的标题和按钮；密码不出现在 `Event::State` 的任何夹具里。Commit。

### Task 10: WinUI

**Files:** `windows/CellGlide.Core/Models/*.cs`、`ViewModels/*`、`windows/CellGlide.App/Pages/OverviewPage.*`、`SettingsPage.*`、`Tray/PanelWindow.*`、`windows/CellGlide.Tests/*`

- 概览页和托盘面板：`wireless.offered` 时在主按钮下显示次要按钮「无线连接」；进行中时显示步骤卡片（网络名、密码带复制按钮、三步、取消）；`Pairing` 时弹 `ContentDialog`：「在 iPhone 上确认验证码 384 271」，主按钮「一致」，次按钮「取消」；对话框随阶段离开 `Pairing` 自动关闭。已连接时状态行用 `wireless_line`。失败时显示原因和「重试」。
- 设置页：「无线连接」开关（做法同严格 DNS 开关：`ServiceToggle.ShouldSend`），说明文字，失败原因；开关下的「已配对的 iPhone」列表（名字、方式、过期标记、移除）；「重新生成网络名和密码」（先确认）。
- [ ] 测试：`ViewModelTests`（`ScriptedCore`）覆盖开始、取消、确认、移除、阶段切换时对话框的开合；`ContractTests`、`AppKeysTests`、`XBindTests` 通过；`scripts/check-windows-core.ps1` 通过。界面截图由主持人在真机验收时看，不由子代理操作界面。Commit。

### Task 11: 自检、脚本与规定

**Files:** `crates/cg-service/src/windows/doctor.rs`、`crates/cg-ipc`（`CheckId::WirelessAccessPoint`）、`scripts/network-snapshot.ps1`、`scripts/tests/test-network-snapshot.ps1`、`packaging/windows/uninstall.ps1`、`packaging/windows/tests/Uninstall.Tests.ps1`、`AGENTS.md`、`docs/specs/2026-11-13-windows-wireless-design.md`

- doctor：开关打开时才列「这台电脑能开无线接入点」；探测方式同任务 7；无线会话进行中不探测，直接报可用。
- 快照脚本：拍快照时把描述匹配 `Microsoft Wi-Fi Direct Virtual Adapter*` 的网卡的别名和 GUID 写进快照的新字段 `excluded`；`Test-Excluded` 除 cellglide0 外也排除两份快照里 `excluded` 的并集。测试：注入这类网卡的 DNS、接口、路由、注册表条目，期望 `identical`；没有 `excluded` 字段的旧快照仍可比较。
- 卸载脚本：普通卸载也删除 `C:\ProgramData\CellGlide\wireless.json`、`wireless-keys.bin`、`wireless-keys.bin.bad`（用现有的不跟随链接的删除函数），其余数据照旧保留；Pester 测试补上。
- AGENTS.md：加入设计 §11 的条款，并把「This is the only system-wide network effect allowed」改成与接入点并列。**这一步改之前由主持人向用户确认。**
- 设计文档：把与代码不符的几处改正（两种语言；接入点名和密码在 `WirelessStore`；健康档是新加的；发现以 Raw 为主；iPhone 被其他已记住的 Wi‑Fi 抢走是已知限制）。
- [ ] 各自的测试通过，Commit。

### Task 12: 真机验收与文档（主持人和用户）

- [ ] 按设计 §10 的八项逐项做，记进 `docs/validation/windows-1.1.md`；另加：
  - 无线 30 分钟：`cellglide speedtest`、网页、视频；每 5 分钟记一次延迟；iPhone 掉电；期间数据连接有没有断（这是闸门里没测的）。
  - 没装 Apple Devices 的情形（停掉 Apple Mobile Device Service）下无线仍可用。
  - 附近有已记住 Wi‑Fi 时 iPhone 被抢走：记录现象和界面给出的提示是否足够。
  - 每一步前后和会话中的网络快照。
- [ ] `docs/release/` 的 1.1.0 说明、官网文案的素材（官网在 CellProxy 仓库，另行处理）。
- [ ] 最终整分支审查（最强的模型），修完再问用户是否合并。

## 不在本计划

- iPhone App 自动加入接入点（`NEHotspotConfiguration`）：解决「被其他 Wi‑Fi 抢走」和「要去设置里手动连」，要改 CellProxy，另行设计。
- 拔线后自动转无线。
