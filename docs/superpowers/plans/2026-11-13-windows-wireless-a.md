# Windows 无线连接 A：闸门、协议核心、插线配对 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 证明系统服务能开接入点并用 OpenSSL 和 iPhone 握手（闸门），然后做出平台无关的无线协议核心、配对存储和插线自动配对。

**Architecture:** 协议、配对和存储在 `cg-core::wireless`（全平台编译和测试，纯 Rust 加密库）。Windows 专有的接入点、发现、TLS 在 `cg-platform::windows`，经 `cg-core::wireless::ports` 里的 trait 注入。本计划结束时用户能看到的只有一件事：插线时自动完成配对。接入点会话、验证码配对的界面、设置开关的界面在计划 B，闸门通过后再写。

**Tech Stack:** Rust（x25519-dalek、sha2、hmac、hkdf、base64、subtle、getrandom；Windows 上 openssl vendored、windows-rs 的 WinRT WiFiDirect 和 Win32 DNS‑SD、DPAPI）。

**Spec:** `docs/specs/2026-11-13-windows-wireless-design.md`。协议的权威来源是 CellProxy 仓库 `C:\Users\iloss\CellProxy\Sources\SharedControl\Wireless\*.swift`（只读，不改）。

## Global Constraints

- iPhone 端协议一个字节都不改；本仓库不动 CellProxy。
- 只动 CellGlide 自己的东西：不碰用户的 Wi‑Fi 连接、系统移动热点、Internet 连接共享、其他网卡的任何设置；不在 Wi‑Fi Direct 虚拟网卡上写地址、DNS、路由、跃点。
- 接入点只在测试程序或（计划 B 的）无线会话期间存在，停止和进程退出时消失。
- USB 行为不变：每个任务结束时 `cargo test --workspace --locked` 全过，`cargo clippy --workspace --all-targets --locked -- -D warnings` 和 `cargo fmt --all --check` 干净。
- 子代理不得发送合成的键盘鼠标输入，不得改系统主题或设置。
- 新依赖取当前最新稳定版，写进 `Cargo.toml` 的 `[workspace.dependencies]` 和 `Cargo.lock`；`bash scripts/update-licenses.sh` 重新生成许可清单（在 Linux 或 CI 的 linux 任务里核对）。
- 提交用已配置的 noreply 身份；`windows` 分支可以直接推送；不推 main、不发布。
- 文案三处同步：`crates/cg-i18n/locales/en.yml`、`zh-CN.yml` 和 `crates/cg-i18n/src/lib.rs` 的键清单。
- 固定值（与 Swift 源码逐字一致）：
  - 服务类型 `_cellglide-p2p._tcp`；允许的端口 9876、8088、8089、8090、8091；插线配对端口 8092。
  - 请求行 `CG1 DATA <port>\n`、`CG1 PAIR\n`，含换行不超过 32 字节。
  - 配对身份 `CG1-pair`，密钥 SHA256("CG1 public pairing key") = `856878c3285976f206f8bff2c7c253467ce8a8db76b8c7e3db87999b4ec09df1`。
  - 链路密钥 HKDF‑SHA256(ikm=长期密钥, salt 空, info="CG2 link psk", 32)；证明密钥同法，info="CG2 pairing proof"。
  - 长期密钥 HKDF‑SHA256(ikm=X25519 共享秘密, salt=手机随机数‖电脑随机数, info="CG1 long-term key", 32)。
  - 验证码：SHA256("CG1 code"‖手机公钥‖电脑公钥‖手机随机数‖电脑随机数) 前 4 字节大端，模 1 000 000，补零到 6 位。
  - 证明：HMAC‑SHA256(证明密钥, 标签‖记录哈希)，标签 "CG1 phone ok" / "CG1 mac ok"。
  - 行长：配对行含换行小于 512 字节；插线请求小于 256；插线回复小于 512。标识 1–64 个 `[A-Za-z0-9-]`；名字编码前截到 120 个 UTF‑8 字节。
  - 时限：电脑每步等 60 秒；确认窗口 75 秒；插线回复等 70 秒；配对后 10 秒内的 unknown identity 重试。
  - 能力名 `wireless-v2`。

## Review Focus

1. 低阶或全零的对端 X25519 公钥：必须在显示验证码之前中止，不得算出长期密钥（任务 3 的测试）。
2. 非规范输入：带填充或非规范的 base64url、多余空格、超长行、端口带前导零或符号，一律拒绝而不是宽容解析（任务 2、4、5 的测试）。
3. 密钥落盘：`wireless.json` 里不得出现任何密钥字节；密钥文件写入中途崩溃不得留下半个文件（任务 6 的测试）。
4. 插线配对失败（超时、畸形回复、手机拒绝）不得影响正在进行的 USB 会话，也不得保存任何东西（任务 8 的测试）。
5. `C` 行的 phoneID 与实际连上的实例名不一致时中止，不得替换已有的好配对（任务 5 的测试）。

---

### Task 1: 闸门程序（服务身份下开接入点、发现、握手）

**Files:**
- Create: `crates/cg-platform/src/windows/access_point.rs`、`crates/cg-platform/src/windows/mdns.rs`、`crates/cg-platform/src/windows/wireless_tls.rs`
- Create: `crates/cg-platform/examples/wireless_gate.rs`
- Modify: `crates/cg-platform/src/windows/mod.rs`、`crates/cg-platform/Cargo.toml`、根 `Cargo.toml`
- Create: `docs/validation/windows-1.1-gate.md`

**Interfaces:**
- Produces（计划 B 会用，签名在这里定下）：
  - `access_point::AccessPoint::start(ssid: &str, passphrase: &str) -> Result<AccessPoint, ApError>`；`fn clients(&self) -> Vec<(IpAddr /*local*/, IpAddr /*remote*/)>`；`Drop` 和 `fn stop(self)` 关闭。`ApError::{NoAdapter, RadioOff, InUse, Unsupported(String)}`。
  - `mdns::browse(interface_local_ip: IpAddr, timeout: Duration) -> Result<Vec<FoundPhone>, io::Error>`，`FoundPhone { instance: String, address: IpAddr, port: u16 }`。
  - `wireless_tls::connect(address: SocketAddr, identity: &str, psk: &[u8; 32]) -> Result<TlsStream, TlsError>`，`TlsError::{UnknownIdentity, WrongSuite, Io(String)}`；`TlsStream: AsyncRead + AsyncWrite + Unpin + Send`。

- [ ] **Step 1:** 加依赖：`openssl`（features `vendored`）、`tokio-openssl`，只在 `cfg(windows)` 下；`windows` crate 加 feature `Devices_WiFiDirect`、`Security_Credentials`、`Foundation_Collections`、`Win32_NetworkManagement_Dns`、`Win32_NetworkManagement_WiFi`。本机先装 Strawberry Perl（vendored OpenSSL 需要原生 Perl；`winget install StrawberryPerl.StrawberryPerl`，这是开发工具，不是网络设置）。`cargo build -p cg-platform --locked` 能过。
- [ ] **Step 2:** `access_point.rs`：`WiFiDirectAdvertisementPublisher`，`IsAutonomousGroupOwnerEnabled = true`，`ListenStateDiscoverability = Normal`，`LegacySettings` 设网络名和密码；`WiFiDirectConnectionListener` 收到请求后 `WiFiDirectDevice::FromIdAsync` 接受并保留设备对象，从 `GetConnectionEndpointPairs` 取地址。`Aborted` 的分类按设计 §6.1。参考实现：这次验证用的 C# 程序在 spec §3 描述的调用顺序。
- [ ] **Step 3:** `mdns.rs`：先用 `DnsServiceBrowse` + `DnsServiceResolve`（限定接口索引）。如果在服务身份或这块网卡上不工作，改为在 `interface_local_ip:5353` 上自己发 PTR 查询并解析 PTR/SRV/A 应答（只查不答）。两种做法都要过同一个单元测试：解析一段录下来的 mDNS 应答字节得到实例名、地址、端口（应答样本取自 2026‑11‑13 的验证：实例 `4961212D-61DD-4BD0-9FE9-8989FB6B8952`，SRV 端口 56582，A `192.168.137.205`）。
- [ ] **Step 4:** `wireless_tls.rs`：OpenSSL 客户端，只允许 TLS 1.2 和 `ECDHE-PSK-CHACHA20-POLY1305`，关 session cache 和 ticket，PSK 回调给出 identity 和密钥；握手后核对协商出的套件；对端 alert 115 映射为 `UnknownIdentity`。TCP keepalive 30/10/3。测试：对本地 OpenSSL 服务端（同一套件）握手成功并回显一行；服务端不认身份时得到 `UnknownIdentity`；服务端只开别的套件时失败。
- [ ] **Step 5:** `examples/wireless_gate.rs`：参数 `<ssid> <passphrase> <seconds>`。开接入点 → 每 2 秒打印连入的设备和 `mdns::browse` 的结果 → 发现手机后用 `CG1-pair` 身份握手、发 `CG1 PAIR\n`、打印收到的 `C` 行（不继续配对）→ 到时间或 Ctrl‑C 关闭。全程打日志到标准输出。
- [ ] **Step 6（真机，需要用户在场）:** 先在普通用户身份下跑一遍确认与 11‑13 的结果一致。再以 LocalSystem 跑：用一次性计划任务（`schtasks /create /ru SYSTEM ... /sc once`，跑完删除）启动 `wireless_gate.exe`，输出重定向到文件。用户用 iPhone 连接入点并打开 CellGlide。前后各拍一次 `scripts/network-snapshot.ps1`。
- [ ] **Step 7:** 把结果写进 `docs/validation/windows-1.1-gate.md`：两种身份下接入点、发现（用的是哪种做法）、握手各自成不成；快照差异；强杀进程后接入点是否消失。
- [ ] **Step 8:** 推到 `windows` 分支，确认 CI 的 `windows-rust` 和 `windows-app` 在 x64 和 ARM64 上都编过（OpenSSL vendored），记下 CI 时间的变化。许可清单：`scripts/update-licenses.sh --check` 在 linux 任务里通过（需要时先重新生成）。
- [ ] **Step 9:** Commit。

**闸门判定：** LocalSystem 下接入点、发现、握手三项都成，且两种架构的 CI 都过 → 继续。否则**停下来报告**，不要自行换方案（备选见设计 §12）。任务 2–8 不依赖闸门结果，可以先做。

### Task 2: 请求行与基础编码

**Files:**
- Create: `crates/cg-core/src/wireless/mod.rs`、`request.rs`、`encoding.rs`
- Modify: `crates/cg-core/src/lib.rs`、`crates/cg-core/Cargo.toml`、根 `Cargo.toml`

**Interfaces:**
- Produces:
  - `wireless::SERVICE_TYPE: &str`、`ALLOWED_PORTS: [u16; 5]`、`CABLE_PAIRING_PORT: u16 = 8092`、`CAPABILITY: &str = "wireless-v2"`。
  - `request::Request::{Data(u16), Pair}`，`fn line(&self) -> String`，`fn parse(line: &str) -> Option<Request>`（`line` 不含换行）。
  - `encoding::base64url(&[u8]) -> String`、`decode_base64url(&str) -> Option<Vec<u8>>`（只收规范、无填充的）、`is_token(&str) -> bool`、`encode_name(&str) -> String`、`decode_name(&str) -> Option<String>`、`clean_name(&str) -> String`（去控制字符、按字符边界截到 120 字节）、`display_name(&str) -> String`（去引号、控制和格式字符，折叠空白，最多 40 字符；引号集合照抄 `WirelessCableMessage.swift` 的 `quotes`）。

- [ ] **Step 1: 写失败的测试**（移植 `Tests/DesktopCoreTests/WirelessRequestTests.swift` 的全部用例，另加）：
  - `Request::Data(9876).line() == "CG1 DATA 9876\n"`；`parse("CG1 PAIR") == Some(Pair)`。
  - 拒绝：`"CG1 DATA 22"`、`"CG1 DATA 09876"`、`"CG1 DATA +9876"`、`"CG1 DATA 9876 "`、`"cg1 pair"`、32 字节以上的行。
  - `decode_base64url("YQ==")`、`"YQ="`、`"Y"`、`"Y+"` 都是 `None`；`decode_base64url("YQ") == Some(b"a")`；`"YR"`（非规范尾位）是 `None`。
  - `is_token`：空串、65 个字符、含 `_` 或空格的为假。
  - `display_name("  “Eve”\u{202E} 的\tiPhone  ") == "Eve 的 iPhone"`；41 个字符的名字截到 40。
  - `clean_name` 不在多字节字符中间截断（121 字节、末尾是 3 字节汉字的输入）。
- [ ] **Step 2:** `cargo test -p cg-core wireless::` 失败（模块不存在）。
- [ ] **Step 3:** 实现。依赖只加 `base64`。
- [ ] **Step 4:** 测试通过。
- [ ] **Step 5:** Commit。

### Task 3: 配对计算（X25519、承诺、验证码、密钥派生）

**Files:**
- Create: `crates/cg-core/src/wireless/pairing.rs`
- Modify: `crates/cg-core/Cargo.toml`（`x25519-dalek`（feature `static_secrets`）、`sha2`、`hmac`、`hkdf`、`subtle`、`getrandom`、`zeroize`）

**Interfaces:**
- Consumes: 无。
- Produces:
  - `pairing::Role::{Phone, Computer}`。
  - `pairing::Session::new(role) -> Session`；`#[cfg(test)] Session::fixed(role, secret: [u8; 32], nonce: [u8; 16])`；`fn public_key(&self) -> [u8; 32]`；`fn nonce(&self) -> [u8; 16]`；`fn commitment(&self) -> [u8; 32]`。
  - `pairing::verify_commitment(commitment: &[u8], public_key: &[u8], nonce: &[u8]) -> bool`。
  - `Session::agree(self, peer_public: &[u8], peer_nonce: &[u8]) -> Result<Agreed, PairingError>`：一次完成密钥协商（共享秘密全零即低阶公钥 → `PairingError::WeakKey`；长度不对 → `Malformed`）。`Agreed { pub code: String, pub long_term_key: Key }`。
  - `pairing::Key`（32 字节，`Zeroize`，`Debug` 不打印内容）；`pairing::link_key(&Key) -> Key`；`pairing::proof_key(&Key) -> Key`；`pairing::pairing_channel_key() -> Key`；`PAIRING_IDENTITY: &str = "CG1-pair"`。

- [ ] **Step 1: 写失败的测试**（向量来自 `WirelessPairingTests.swift`）：
  - 手机私钥 `77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a`，随机数 `0x10..0x20`；电脑私钥 `5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb`，随机数 `0xA0..0xB0`。
  - 手机公钥 `8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a`；电脑公钥 `de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f`。
  - 手机承诺 `37d59543b75c0e4183ad43b8494d76c75e190a1c3e1aad56ebbf70a130e62793`。
  - 两边 `agree` 的验证码都是 `"423265"`，长期密钥都是 `16cc58fa39be78a73899d87b0caeaa45f12d6d9ac0314973df3ec6d0a3ea3c43`。
  - `pairing_channel_key()` 等于 Global Constraints 里的十六进制值。
  - 替换任一方公钥（中间人）时两边验证码不同。
  - 对端公钥全零 → `WeakKey`；5 字节公钥、4 字节随机数 → `Malformed`。
  - `verify_commitment` 对改过一位的公钥或随机数为假。
  - `link_key(k) != proof_key(k) != k`（三者互不相同）。
- [ ] **Step 2:** 运行，失败。
- [ ] **Step 3:** 实现。`link_key` / `proof_key` 的 HKDF salt 为空（CryptoKit 的 `HKDF.deriveKey` 不给 salt 时等同全零 salt，即 RFC 5869 的缺省）。写一条测试把 `link_key(固定长期密钥)` 钉成一个十六进制值，值用独立的 Python（`hmac`/`hashlib`）算出并写在测试注释里。
- [ ] **Step 4:** 测试通过。
- [ ] **Step 5:** Commit。

### Task 4: 插线配对的消息

**Files:**
- Create: `crates/cg-core/src/wireless/cable.rs`

**Interfaces:**
- Consumes: `encoding::*`、`pairing::Key`。
- Produces:
  - `cable::request(computer_id: &str, computer_name: &str) -> String`（`"CG1 CABLE <id> <name>\n"`）。
  - `cable::Reply::{Known { phone_id: String }, New { phone_id: String, identity: String, key: Key, phone_name: String }}`。
  - `cable::parse_reply(line: &str) -> Option<Reply>`（`line` 不含换行；密钥字段是**标准** base64 带填充，其余同 Swift）。
  - `cable::MAX_REQUEST: usize = 256`、`MAX_REPLY: usize = 512`、`REPLY_TIMEOUT: Duration = 70 s`。
  - `encoding::is_valid_identity(&str) -> bool`：规则照 `WirelessKeychain.isValidIdentity`（实现前读 `WirelessKeychain.swift` 确认）。

- [ ] **Step 1: 写失败的测试**（移植 `WirelessCablePairingTests.swift` 里与消息有关的用例）：请求行格式；120 字节以上的名字被截断后整行仍小于 256；`parse_reply("CG1 KNOWN ABC-1")`；新配对回复四个字段解析正确且名字经 `clean_name`；拒绝：字段数不对、密钥不是 32 字节、密钥用了 base64url 字符、identity 非法、512 字节以上。
- [ ] **Step 2–4:** 失败 → 实现 → 通过。
- [ ] **Step 5:** Commit。

### Task 5: 验证码配对的消息与状态机

**Files:**
- Create: `crates/cg-core/src/wireless/code_message.rs`、`crates/cg-core/src/wireless/code_pairing.rs`

**Interfaces:**
- Consumes: `pairing::*`、`encoding::*`、`request::Request`。
- Produces:
  - `code_message`：`parse_commitment`、`computer(public_key, nonce, id, name) -> String`、`parse_phone`、`parse_answer -> Option<Answer>`（`Answer::{Allowed { identity, phone_name, proof, signed }, Denied}`）、`finished(proof) -> String`、`Transcript`（`add(&str)`、`digest()`）、`proof(side, &Key, &Transcript) -> [u8; 32]`、`verify(...) -> bool`（常数时间比较）。
  - `code_pairing::run<S, C>(stream: S, expected_phone_id: &str, computer_id: &str, computer_name: &str, confirm: C) -> Result<Paired, CodePairingError>`，`S: AsyncRead + AsyncWrite + Unpin`，`C: FnOnce(String /*6 位验证码*/) -> impl Future<Output = bool>`（用户点「一致」为真）。`Paired { phone_id, identity, phone_name, long_term_key }`。调用方在 `run` 返回 `Ok` **之前**无法保存，所以 `run` 内部的顺序是：收到 `OK` 且证明正确、且 `confirm` 为真 → 调用 `save: impl FnOnce(&Paired) -> io::Result<()>`（作为第六个参数）→ 再发 `F`。
  - `CodePairingError::{Denied, Cancelled, Timeout, WrongPhone, BadCommitment, BadProof, WeakKey, Malformed, Io(String)}`。
  - 常量 `STEP_TIMEOUT = 60 s`、`CONFIRM_WINDOW = 75 s`。

- [ ] **Step 1: 写失败的测试**（移植 `WirelessPairingFlowTests.swift`；对端用测试里写的一个「假手机」协程，角色 `Phone`）：
  - 正常流程：两边验证码相同，`save` 被调用恰好一次且在 `F` 发出之前（假手机在收到 `F` 前检查一个标志）；`F` 的证明能被假手机验证。
  - `C` 行的 phoneID 不等于 `expected_phone_id` → `WrongPhone`，没有发出 `M`。
  - 手机公开的公钥与承诺不符 → `BadCommitment`，`confirm` 没被调用。
  - 手机回 `NO` → `Denied`；`OK` 的证明错 → `BadProof`；两种都不调用 `save`。
  - `confirm` 返回假 → `Cancelled`，不调用 `save`，不发 `F`。
  - 手机先回 `OK`、用户后确认，和用户先确认、手机后回 `OK`，两种先后都成功。
  - 改写记录里任一行的一个字节（在假手机和被测方之间的转发器里改）→ `BadProof` 或验证码不同。
  - 512 字节以上的行、字段数不对的行 → `Malformed`。
  - 时间用 `tokio::time::pause`：手机不回 `P` 60 秒 → `Timeout`；显示验证码后 75 秒没等齐 → `Timeout`。
  - 收到 `C` 后**立即**发 `M`（假手机只等 15 秒）。
- [ ] **Step 2–4:** 失败 → 实现 → 通过。
- [ ] **Step 5:** Commit。

### Task 6: 配对存储

**Files:**
- Create: `crates/cg-core/src/wireless/store.rs`
- Create: `crates/cg-platform/src/windows/secrets.rs`
- Modify: `crates/cg-core/src/wireless/mod.rs`

**Interfaces:**
- Produces:
  - `store::SecretStore` trait：`fn protect(&self, plain: &[u8]) -> io::Result<Vec<u8>>`、`fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>>`。测试用 `store::PlainSecrets`（`#[cfg(any(test, feature = "testing"))]`，只做异或，明确不是加密）。
  - `store::Record { peer_id, name, identity, created_at: u64 /*Unix 秒*/, method: Method::{Code, Cable}, stale: bool }`。
  - `store::WirelessStore::open(dir: &Path, secrets: Arc<dyn SecretStore>) -> io::Result<WirelessStore>`；`local_id() -> String`（首次生成随机 UUID 大写并保存）；`records() -> Vec<Record>`；`key(identity) -> Option<Key>`；`save(record, key) -> io::Result<()>`（同一 `peer_id` 的旧记录和旧密钥在新记录写成功后才替换）；`remove(peer_id)`；`set_stale(peer_id, bool)`；`remember_phone(usb_serial, phone_id)`；`phone_id(usb_serial) -> Option<String>`；`access_point() -> io::Result<(String /*ssid*/, String /*passphrase*/)>`（首次生成：`CellGlide-` 加 4 位 `[A-Z0-9]`，密码 12 位 `[a-z0-9]` 按 4‑4‑4 加连字符）；`regenerate_access_point()`。
  - 文件：`wireless.json`（记录、本机标识、USB 对应、网络名）和 `wireless-keys.bin`（`protect` 过的 JSON：identity → 密钥，接入点密码）。都用「写临时文件再改名」。
  - `cg_platform::windows::secrets::Dpapi`：`CryptProtectData` / `CryptUnprotectData`，`CRYPTPROTECT_LOCAL_MACHINE`，附加熵 `b"CellGlide wireless"`。

- [ ] **Step 1: 写失败的测试**：保存后重开能读回记录和密钥；`wireless.json` 的字节里搜不到密钥的任何 8 字节片段，也搜不到接入点密码；`remove` 同时删记录和密钥；`set_stale` 持久；同一 `peer_id` 再 `save` 后旧 identity 的密钥没了；`secrets.protect` 出错时 `save` 失败且旧记录原样；`wireless-keys.bin` 损坏时 `open` 成功、记录都标 `stale`（不删）；`local_id` 两次打开相同；`access_point` 两次相同且格式匹配正则，`regenerate` 后不同。Windows 上另加 `Dpapi` 的往返测试和「密文里搜不到明文」。
- [ ] **Step 2–4:** 失败 → 实现 → 通过。
- [ ] **Step 5:** Commit。

### Task 7: 设置开关与能力名

**Files:**
- Modify: `crates/cg-core/src/settings.rs`、`crates/cg-protocol/src/phone.rs`

**Interfaces:**
- Produces: `Stored.wireless_enabled: bool`（缺省 `false`，旧设置文件读入后为 `false`）；`capability::WIRELESS: &str = "wireless-v2"`。

- [ ] **Step 1:** 测试：没有该字段的旧 JSON 读成 `false`；写回再读保持；`Hello::has(capability::WIRELESS)` 对含 `wireless-v2` 的 hello 为真、对 `wireless-v1` 为假。
- [ ] **Step 2–4:** 失败 → 实现 → 通过。开关的 IPC 和界面在计划 B；本计划里用环境变量 `CELLGLIDE_WIRELESS=1` 在服务启动时把它当作打开（仅用于任务 8 的真机验证，写进代码注释，计划 B 删除）。
- [ ] **Step 5:** Commit。

### Task 8: 插线自动配对

**Files:**
- Create: `crates/cg-core/src/wireless/cable_pairing.rs`
- Modify: `crates/cg-core/src/controller.rs`、`crates/cg-core/src/testing/fake_phone.rs`、`crates/cg-service/src/windows/mod.rs`（把 `WirelessStore`（目录 `data_dir()`，`Dpapi`）交给控制器）

**Interfaces:**
- Consumes: `Phone::open(CABLE_PAIRING_PORT)`、`cable::*`、`store::WirelessStore`、`Hello`、`Stored.wireless_enabled`。
- Produces:
  - `cable_pairing::Outcome::{AlreadyPaired, Paired(Record), Skipped}`。
  - `cable_pairing::pair_if_needed(phone: &Phone, hello: &Hello, usb_serial: &str, enabled: bool, computer_name: &str, store: &WirelessStore) -> Result<Outcome>`：开关关着或 hello 没有 `wireless-v2` → `Skipped`（不开连接）；已记住这台 USB 设备对应的手机、有记录、有密钥且不过期 → `AlreadyPaired`（不开连接）；否则发请求，70 秒内读一行：`KNOWN` → 记住对应、清过期标记；新配对 → 名字经 `display_name`，保存。
  - `ControllerConfig.wireless: Option<Arc<WirelessStore>>`。控制器在 USB 会话**建立之后**（`establish` 成功）另起一个任务调用它，每个会话一次；结果只记日志，任何错误都不影响会话。

- [ ] **Step 1: 写失败的测试**（`fake_phone` 加 8092 端口，可配置回 `New`、`KNOWN`、畸形、不回）：
  - 新电脑：保存了记录（方式 `Cable`）和密钥，记住了 USB 对应。
  - 第二个会话：不再连 8092（假手机的连接计数不变）。
  - 过期的记录：再问一次；回 `KNOWN` 清掉过期；回新密钥则替换。
  - 开关关 / hello 无 `wireless-v2`：8092 连接计数为 0。
  - 畸形回复、70 秒不回（`tokio::time::pause`）、连接被拒：返回错误，什么都没保存，**USB 会话仍然是 Connected**（控制器级测试，用现有的 `Rig`）。
  - 请求行恰好是 `cable::request(...)`，之后不再发任何字节。
- [ ] **Step 2–4:** 失败 → 实现 → 通过。
- [ ] **Step 5（真机，需要用户在场）:** 服务带 `CELLGLIDE_WIRELESS=1` 运行，iPhone 关 VPN 模式、插线共享。预期：iPhone 弹「允许“<计算机名>”以后无线连接？」，点允许后 `C:\ProgramData\CellGlide\wireless.json` 多一条记录；iPhone 的「已配对」列表里出现这台电脑；重新插线不再弹。用任务 1 的闸门程序加一个参数 `--data <identity> <hex key>`（从存储读出）做一次 `CG1 DATA 8088` 握手并发 hello 请求，收到 JSON：证明插线配来的密钥能在无线上用。
- [ ] **Step 6:** 结果记进 `docs/validation/windows-1.1-gate.md`。Commit，推 `windows`。

---

## 之后

计划 B（闸门通过后写）：`PhoneLink` 抽象与无线会话、接入点生命周期、发现与选机、重连与切换、验证码配对接入界面、IPC、WinUI 与 CLI、doctor、快照脚本、卸载脚本、AGENTS.md 的例外条款、真机验收。
