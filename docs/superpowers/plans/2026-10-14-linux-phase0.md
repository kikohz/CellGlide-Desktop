# Linux 第 0 阶段技术验证 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Rust 在 Linux 上跑通「插 iPhone → 经手机蜂窝网络上网」：root 服务接管流量和 DNS，命令行与 iced 小界面控制它。

**Architecture:** Cargo workspace，按职责分 crate：`cg-protocol`（纯协议编解码）、`cg-ipc`（控制通道消息）、`cg-core`（tokio 异步：usbmuxd 客户端、手机会话、SOCKS 转发与 UDP 桥、本地 DNS、连接状态机，以及 `TunnelBackend` trait）、`hev-sys`（静态链接 hev-socks5-tunnel 2.18.0）、`cg-platform`（Linux 的 `TunnelBackend`：`/dev/net/tun` + `ip` 命令 + systemd-resolved D-Bus + hev 线程）、`cg-service` / `cg-cli` / `cg-gui` 三个可执行程序。核心逻辑全部可在 macOS 上用假 usbmuxd 和假手机测试；Linux 专有部分在 Docker 和 GitHub Ubuntu 机器上测试。

**Tech Stack:** Rust（edition 2024，stable）、tokio 1.53、tokio-util 0.7、plist 1.10、serde / serde_json、hyper 1.12（http1 client）、zbus 5.19（blocking proxy）、nix 0.31、libc 0.2、clap 4.6、iced 0.14、ksni 0.3、thiserror 2、tracing。

**Spec:** `docs/specs/2026-10-14-linux-phase0-design.md`（Mac 参考实现在 CellProxy 仓库，下文写作 `CellProxy:路径`，本机位于 `~/Work/CellProxy`）。

## Global Constraints

- DNS 只动自己的 TUN 网卡 `cellglide0`：只用 systemd-resolved 的 `SetLinkDNSEx` / `SetLinkDomains` / `SetLinkDefaultRoute` / `RevertLink`；不改 `/etc/resolv.conf`、不改其他网卡、不改 NetworkManager 保存的连接；没有任何「恢复旧 DNS」逻辑（见仓库 `AGENTS.md`）。
- 隧道常量与 `CellProxy:Sources/DesktopCore/TunnelConfiguration.swift` 完全一致：地址 `198.18.0.1`、`fd6c:6779:6465::1`；MTU `1500`；路由 `0.0.0.0/1`、`128.0.0.0/1`、`::/1`、`8000::/1`；hev `tcp-read-write-timeout: 1800000`、`udp-read-write-timeout: 30000`；公共 DNS 回退 `2606:4700:4700::1111`、`2001:4860:4860::8888`、`1.1.1.1`、`8.8.8.8`。
- 本地解析器默认 `127.0.0.1:1053`（Task 2 的 CI 探测若证明 resolved 不接受，改为 `198.18.0.1:53`，只改 `cg-core/src/tunnel.rs` 的 `RESOLVER` 常量）。
- 手机协议不改：端口 SOCKS `9876`、控制 `8088`、状态 `8089`、指标 `8090`；握手 `service == "CellProxy"`、`protocolVersion == 1`、`sessionID` 为 UUID、必须有 `socks5-tcp`；建隧道还必须有 `udp-over-usb-v1`。
- 转发中的 TCP / UDP 连接不设读超时（Mac 0.5.7 曾因 5 秒超时切断安静连接）；超时只用于 usbmuxd 请求（5 秒）、握手、指标静默（5 秒）、DNS 上游（10 秒）、出口检测（8 秒）。
- 并发上限与 Mac 一致：本地 SOCKS 客户端 256、DNS 在途问题 128、DNS TCP 客户端 16。
- 网卡名 `cellglide0`；控制 socket `/run/cellglide/control.sock`，权限 `0660`，组 `cellglide`。
- 面向用户的文字用简体中文；代码注释用英文、少而精。
- 依赖用 `[workspace.dependencies]` 统一版本；不引入 `rtnetlink`、`tun2proxy`、`reqwest`。
- 每个任务结束前运行 `cargo fmt --all` 与 `cargo clippy --workspace --all-targets -- -D warnings`，全部通过再提交。
- 提交信息末尾加 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。

## Review Focus

1. **usbmuxd 自身重启或 socket 断开**：Listen 连接 EOF 时，状态机必须当作所有手机都已拔出、拆掉隧道，并在 2 秒后重新 Listen。测试放在 Task 11（`usbmuxd_restart_tears_down_and_relistens`）。
2. **服务被 `kill -9`**：不得留下 `cellglide0`、路由或 resolved 里的单网卡 DNS。测试放在 Task 13（`killed_holder_leaves_nothing`，特权测试）。
3. **本地 DNS 端口被占用**：连接失败并给出中文原因，且没有任何网卡、路由留下。测试放在 Task 11（`resolver_port_busy_fails_cleanly`）。
4. **同时插着两部 iPhone**：状态为「选择手机」，不得自动挑一部；`connect --serial` 指定后才连接。测试放在 Task 11（`two_phones_require_choice`）。
5. **控制 socket 收到超长或乱码行**：回 `error` 响应或断开该连接，服务继续服务其他客户端。测试放在 Task 14（`garbage_line_gets_error_and_server_survives`）。

## 文件结构

```
Cargo.toml                         workspace 与统一依赖版本
rust-toolchain.toml                stable + rustfmt + clippy
.gitmodules                        hev 子模块
.github/workflows/ci.yml           fmt / clippy / test / Windows check / 特权集成测试 / resolved 探测
scripts/linux-test.sh              在 Docker 里跑 Linux 测试（macOS 开发用）
scripts/probe-resolved.sh          验证 resolved 是否接受「回环地址 + 非 53 端口」
scripts/network-snapshot.sh        打印「除 cellglide0 外」的 DNS / 路由 / 配置文件指纹
scripts/install-dev.sh             开发安装：二进制、systemd 单元、cellglide 组
packaging/cellglide.service        systemd 单元
docs/validation/linux-phase0.md    第 0 阶段结论文档（实机测完填写）
crates/cg-protocol/src/{lib,mux,phone,socks}.rs
crates/cg-ipc/src/{lib,client}.rs
crates/cg-core/src/{lib,error,mux,phone,forwarder,udp_bridge,resolver,doh,tunnel,controller}.rs
crates/cg-core/src/testing/{mod,fake_mux,fake_phone,fake_backend}.rs   （feature = "testing"）
crates/hev-sys/{build.rs,src/lib.rs,hev/}                                （hev 为 git 子模块）
crates/cg-platform/src/{lib,tun,ip,resolved,hev,backend}.rs + examples/hold_tunnel.rs + tests/privileged.rs
crates/cg-service/src/{main,server,auth,doctor}.rs
crates/cg-cli/src/{main,render}.rs
crates/cg-gui/src/{main,app,tray}.rs
```

---
### Task 1: 工具链、workspace 骨架与 CI

**Files:**
- Create: `rust-toolchain.toml`, `Cargo.toml`, `crates/cg-protocol/Cargo.toml`, `crates/cg-protocol/src/lib.rs`, `.github/workflows/ci.yml`, `scripts/linux-test.sh`
- Modify: `.gitignore`

**Interfaces:**
- Produces: workspace 根 `Cargo.toml` 的 `[workspace.dependencies]`（后续 crate 一律 `xxx.workspace = true`）；`cg_protocol::ProtocolError`。

- [ ] **Step 1: 安装 Rust（本机还没有）**

Run: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal && . "$HOME/.cargo/env" && rustc --version`
Expected: 输出 `rustc 1.9x.x`（≥ 1.85，edition 2024 需要）。

- [ ] **Step 2: 写工具链与 workspace 文件**

`rust-toolchain.toml`：

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

`Cargo.toml`：

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.0.1"
edition = "2024"
license = "UNLICENSED"
publish = false

[workspace.dependencies]
cg-protocol = { path = "crates/cg-protocol" }
cg-ipc = { path = "crates/cg-ipc" }
cg-core = { path = "crates/cg-core" }
cg-platform = { path = "crates/cg-platform" }
hev-sys = { path = "crates/hev-sys" }
anyhow = "1.0"
bytes = "1.12"
cc = "1.8"
clap = { version = "4.6", features = ["derive"] }
futures = "0.3"
http-body-util = "0.1"
hyper = { version = "1.12", features = ["client", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
iced = { version = "0.14", features = ["tokio"] }
ksni = "0.3"
libc = "0.2"
nix = { version = "0.31", features = ["user", "fs", "hostname"] }
plist = "1.10"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tempfile = "3"
thiserror = "2.0"
tokio = { version = "1.53", features = ["full"] }
tokio-util = "0.7"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1.28", features = ["v4"] }
zbus = { version = "5.19", default-features = false, features = ["tokio", "blocking-api"] }
```

`crates/cg-protocol/Cargo.toml`：

```toml
[package]
name = "cg-protocol"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
plist.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
uuid.workspace = true
```

`crates/cg-protocol/src/lib.rs`：

```rust
//! Wire formats shared with the iPhone app and usbmuxd. No IO.

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("plist: {0}")]
    Plist(#[from] plist::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
    #[error("message too large")]
    TooLarge,
    #[error("手机 CellGlide 版本不兼容，请更新手机 App")]
    Incompatible,
}

#[cfg(test)]
mod tests {
    #[test]
    fn error_text_is_chinese() {
        assert!(super::ProtocolError::Incompatible.to_string().contains("更新手机 App"));
    }
}
```

`.gitignore` 追加 `/target` 已有，无需改动；若缺失则加上。

- [ ] **Step 3: 跑测试**

Run: `cargo test --workspace`
Expected: `test tests::error_text_is_chinese ... ok`

- [ ] **Step 4: Docker 里跑 Linux 测试的脚本**

`scripts/linux-test.sh`（macOS 开发时用；`--privileged` 让以后的 TUN 测试也能跑，resolved 测试只在 CI 跑）：

```sh
#!/bin/sh
# Runs the workspace tests in a Linux container. Usage: scripts/linux-test.sh [cargo test args...]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
docker run --rm --privileged \
    -v "$ROOT":/src -w /src \
    -v cellglide-cargo:/usr/local/cargo/registry \
    -v cellglide-target:/src/target \
    rust:1-bookworm \
    sh -c 'apt-get update -qq && apt-get install -y -qq iproute2 >/dev/null && cargo test --workspace "$@"' sh "$@"
```

Run: `chmod +x scripts/linux-test.sh && colima status || colima start; scripts/linux-test.sh`
Expected: 同 Step 3，`1 passed`。

- [ ] **Step 5: CI 工作流**

`.github/workflows/ci.yml`：

```yaml
name: ci
on:
  push:
  pull_request:

jobs:
  linux:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v5
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  windows-check:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --workspace
```

- [ ] **Step 6: 格式检查并提交**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告。

```bash
git add rust-toolchain.toml Cargo.toml crates/cg-protocol .github/workflows/ci.yml scripts/linux-test.sh .gitignore
git commit -m "Workspace skeleton, CI and Docker test script

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: 验证 systemd-resolved 是否接受「回环地址 + 非 53 端口」

设计文档第 4 节的首个待验证项。结论决定 `RESOLVER` 常量（Task 10）。

**Files:**
- Create: `scripts/probe-resolved.sh`, `scripts/network-snapshot.sh`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `scripts/network-snapshot.sh`（Task 13 的特权测试和实机测试都用它）；CI 日志中的 `RESULT:` 行。

- [ ] **Step 1: 快照脚本**

`scripts/network-snapshot.sh`：

```sh
#!/bin/sh
# Prints everything CellGlide must never change: DNS and routes of every other link,
# plus fingerprints of resolv.conf and saved NetworkManager connections.
set -eu
EXCLUDE=${1:-cellglide0}
echo "## resolvectl dns"; resolvectl dns 2>/dev/null | grep -v "($EXCLUDE)" || true
echo "## resolvectl domain"; resolvectl domain 2>/dev/null | grep -v "($EXCLUDE)" || true
echo "## routes v4"; ip -4 route show table all | grep -v "dev $EXCLUDE" || true
echo "## routes v6"; ip -6 route show table all | grep -v "dev $EXCLUDE" | grep -v "^local\|^multicast\|^ff00::/8" || true
echo "## resolv.conf"; sha256sum /etc/resolv.conf "$(readlink -f /etc/resolv.conf)" 2>/dev/null || true
echo "## NetworkManager"; sha256sum /etc/NetworkManager/system-connections/* 2>/dev/null || true
```

- [ ] **Step 2: 探测脚本**

`scripts/probe-resolved.sh`（需要 root；退出码 0 = 接受，2 = 不接受，1 = 其他网卡被改动或撤销不干净）：

```bash
#!/bin/bash
# Does systemd-resolved accept 127.0.0.1:1053 as the DNS server of one TUN link,
# without touching any other link? Prints a RESULT line for the phase 0 report.
set -eu
IF=cgprobe0
DIR=$(cd "$(dirname "$0")" && pwd)
BUS="org.freedesktop.resolve1 /org/freedesktop/resolve1 org.freedesktop.resolve1.Manager"
before=$("$DIR/network-snapshot.sh" "$IF")
ip tuntap add dev "$IF" mode tun
trap 'ip link del "$IF" 2>/dev/null || true' EXIT
ip link set "$IF" up
IDX=$(cat "/sys/class/net/$IF/ifindex")
# a(iayqs): family AF_INET=2, 4 address bytes, port, server name
busctl call $BUS SetLinkDNSEx 'ia(iayqs)' "$IDX" 1 2 4 127 0 0 1 1053 ""
busctl call $BUS SetLinkDomains 'ia(sb)' "$IDX" 1 "." true
busctl call $BUS SetLinkDefaultRoute 'ib' "$IDX" true
resolvectl dns "$IF"
accepted=yes
resolvectl dns "$IF" | grep -q "127.0.0.1:1053" || accepted=no
during=$("$DIR/network-snapshot.sh" "$IF")
busctl call $BUS RevertLink 'i' "$IDX"
ip link del "$IF"
trap - EXIT
after=$("$DIR/network-snapshot.sh" "$IF")
if [ "$before" != "$during" ] || [ "$before" != "$after" ]; then
    echo "RESULT: other links changed"
    diff <(echo "$before") <(echo "$during") || true
    diff <(echo "$before") <(echo "$after") || true
    exit 1
fi
if resolvectl dns 2>/dev/null | grep -q "127.0.0.1:1053"; then
    echo "RESULT: still listed after RevertLink"
    exit 1
fi
if [ "$accepted" = no ]; then
    echo "RESULT: loopback with port rejected"
    exit 2
fi
echo "RESULT: loopback with port accepted; other links untouched"
```

- [ ] **Step 3: 加 CI 任务**

在 `.github/workflows/ci.yml` 的 `jobs:` 下追加：

```yaml
  resolved-probe:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v5
      - run: chmod +x scripts/*.sh && sudo scripts/probe-resolved.sh
```

- [ ] **Step 4: 推送并读结果**

Run: `chmod +x scripts/*.sh && git add scripts .github && git commit -m "Probe systemd-resolved per-link DNS with a loopback port

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>" && git push && sleep 5 && gh run watch --exit-status $(gh run list --workflow ci --limit 1 --json databaseId -q '.[0].databaseId')`
Expected: `resolved-probe` 任务日志里有 `RESULT: loopback with port accepted; other links untouched`。

- [ ] **Step 5: 记录结论**

若输出为 `rejected`：在 Task 10 中把 `RESOLVER` 改成 `198.18.0.1:53`（其余代码不变）。无论哪种结论，把 `RESULT:` 行抄进 `docs/validation/linux-phase0.md`（Task 17 创建；此处先记在 Task 17 的待填表格里，或现在建一个只有这一行的草稿文件并提交）。

```bash
mkdir -p docs/validation && printf '# Linux 第 0 阶段结论\n\n## resolved 回环端口验证\n\n- CI ubuntu-24.04：<粘贴 RESULT 行>\n' > docs/validation/linux-phase0.md
git add docs/validation/linux-phase0.md && git commit -m "Record resolved probe result

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: usbmuxd 协议编解码（`cg-protocol::mux`）

参照 `CellProxy:Sources/DesktopCore/MuxSocket.swift`、`USBTransport.swift`（`USBDiscovery`）、`USBEventMonitor.swift`。

**Files:**
- Create: `crates/cg-protocol/src/mux.rs`
- Modify: `crates/cg-protocol/src/lib.rs`（加 `pub mod mux;`）

**Interfaces:**
- Produces:
  - `pub const HEADER_LEN: usize = 16;` `pub const MAX_MESSAGE: u32 = 1_048_576;`
  - `pub struct Device { pub serial: String, pub mux_id: u32 }`（`Clone, Debug, PartialEq, Eq`）
  - `pub enum MuxEvent { Attached(Device), Detached(u32), Other }`
  - `pub enum MuxRequest { ListDevices, Listen, Connect { mux_id: u32, port: u16 }, ReadPairRecord { serial: String } }`，`fn encode(&self, tag: u32) -> Result<Vec<u8>, ProtocolError>`
  - `pub fn encode_message(dict: &plist::Dictionary, tag: u32) -> Result<Vec<u8>, ProtocolError>`
  - `pub fn body_len(header: &[u8; HEADER_LEN]) -> Result<usize, ProtocolError>`
  - `pub fn decode_body(body: &[u8]) -> Result<plist::Dictionary, ProtocolError>`
  - `pub fn result_number(dict: &plist::Dictionary) -> Option<u64>`
  - `pub fn device_list(dict: &plist::Dictionary) -> Result<Vec<Device>, ProtocolError>`
  - `pub fn parse_event(dict: &plist::Dictionary) -> MuxEvent`
  - `pub fn has_pair_record(dict: &plist::Dictionary) -> bool`

- [ ] **Step 1: 写失败的测试**

`crates/cg-protocol/src/mux.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value};

    fn int(v: u64) -> Value {
        Value::Integer(v.into())
    }

    fn usb_device(serial: &str, id: u64, kind: &str) -> Value {
        let mut props = Dictionary::new();
        props.insert("ConnectionType".into(), kind.into());
        props.insert("SerialNumber".into(), serial.into());
        let mut item = Dictionary::new();
        item.insert("DeviceID".into(), int(id));
        item.insert("Properties".into(), Value::Dictionary(props));
        Value::Dictionary(item)
    }

    #[test]
    fn connect_header_and_port_byte_order() {
        let bytes = MuxRequest::Connect { mux_id: 7, port: 9876 }.encode(3).unwrap();
        let header: [u8; 16] = bytes[..16].try_into().unwrap();
        assert_eq!(u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize, bytes.len());
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(header[8..12].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(header[12..16].try_into().unwrap()), 3);
        let dict = decode_body(&bytes[16..]).unwrap();
        assert_eq!(dict.get("MessageType").and_then(Value::as_string), Some("Connect"));
        assert_eq!(dict.get("DeviceID").and_then(Value::as_unsigned_integer), Some(7));
        // 9876 = 0x2694, sent as network order inside a little-endian integer: 0x9426.
        assert_eq!(dict.get("PortNumber").and_then(Value::as_unsigned_integer), Some(0x9426));
        assert_eq!(dict.get("kLibUSBMuxVersion").and_then(Value::as_unsigned_integer), Some(3));
    }

    #[test]
    fn body_len_rejects_bad_headers() {
        let good = MuxRequest::ListDevices.encode(1).unwrap();
        let header: [u8; 16] = good[..16].try_into().unwrap();
        assert_eq!(body_len(&header).unwrap(), good.len() - 16);

        let mut wrong_type = header;
        wrong_type[8] = 1;
        assert!(body_len(&wrong_type).is_err());

        let mut too_big = header;
        too_big[0..4].copy_from_slice(&(MAX_MESSAGE + 1).to_le_bytes());
        assert!(body_len(&too_big).is_err());

        let mut too_small = header;
        too_small[0..4].copy_from_slice(&15u32.to_le_bytes());
        assert!(body_len(&too_small).is_err());
    }

    #[test]
    fn device_list_keeps_usb_only_sorted_by_serial() {
        let mut dict = Dictionary::new();
        dict.insert(
            "DeviceList".into(),
            Value::Array(vec![
                usb_device("bbbb", 2, "USB"),
                usb_device("cccc", 3, "Network"),
                usb_device("aaaa", 1, "USB"),
            ]),
        );
        let devices = device_list(&dict).unwrap();
        assert_eq!(
            devices,
            vec![
                Device { serial: "aaaa".into(), mux_id: 1 },
                Device { serial: "bbbb".into(), mux_id: 2 },
            ]
        );
        assert!(device_list(&Dictionary::new()).is_err());
    }

    #[test]
    fn events_and_results() {
        let Value::Dictionary(mut attached) = usb_device("aaaa", 5, "USB") else { unreachable!() };
        attached.insert("MessageType".into(), "Attached".into());
        assert_eq!(parse_event(&attached), MuxEvent::Attached(Device { serial: "aaaa".into(), mux_id: 5 }));

        let Value::Dictionary(mut wifi) = usb_device("aaaa", 6, "Network") else { unreachable!() };
        wifi.insert("MessageType".into(), "Attached".into());
        assert_eq!(parse_event(&wifi), MuxEvent::Other);

        let mut detached = Dictionary::new();
        detached.insert("MessageType".into(), "Detached".into());
        detached.insert("DeviceID".into(), int(5));
        assert_eq!(parse_event(&detached), MuxEvent::Detached(5));

        let mut result = Dictionary::new();
        result.insert("MessageType".into(), "Result".into());
        result.insert("Number".into(), int(0));
        assert_eq!(result_number(&result), Some(0));
        assert!(!has_pair_record(&result));
        result.insert("PairRecordData".into(), Value::Data(vec![1, 2]));
        assert!(has_pair_record(&result));
    }

    #[test]
    fn encode_message_round_trips() {
        let mut dict = Dictionary::new();
        dict.insert("MessageType".into(), "Result".into());
        dict.insert("Number".into(), int(3));
        let bytes = encode_message(&dict, 9).unwrap();
        let header: [u8; 16] = bytes[..16].try_into().unwrap();
        assert_eq!(body_len(&header).unwrap(), bytes.len() - 16);
        assert_eq!(decode_body(&bytes[16..]).unwrap(), dict);
    }
}
```

`lib.rs` 加一行 `pub mod mux;`。

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p cg-protocol mux`
Expected: 编译失败，`cannot find function encode_message` 等。

- [ ] **Step 3: 实现**

在 `mux.rs` 顶部（测试模块之前）：

```rust
//! usbmuxd's plist protocol: a 16-byte little-endian header (length, version 1,
//! type 8 = plist, tag) followed by an XML property list.

use crate::ProtocolError;
use plist::{Dictionary, Value};
use std::io::Cursor;

pub const HEADER_LEN: usize = 16;
pub const MAX_MESSAGE: u32 = 1_048_576;
const VERSION: u32 = 1;
const PLIST_TYPE: u32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The iPhone's UDID as usbmuxd reports it.
    pub serial: String,
    pub mux_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxEvent {
    Attached(Device),
    Detached(u32),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxRequest {
    ListDevices,
    Listen,
    Connect { mux_id: u32, port: u16 },
    ReadPairRecord { serial: String },
}

impl MuxRequest {
    fn fields(&self) -> Dictionary {
        let mut d = Dictionary::new();
        let kind = match self {
            Self::ListDevices => "ListDevices",
            Self::Listen => "Listen",
            Self::Connect { mux_id, port } => {
                d.insert("DeviceID".into(), Value::Integer(u64::from(*mux_id).into()));
                // usbmuxd reads the port in network order from a little-endian integer.
                let wire = u16::from_le_bytes(port.to_be_bytes());
                d.insert("PortNumber".into(), Value::Integer(u64::from(wire).into()));
                "Connect"
            }
            Self::ReadPairRecord { serial } => {
                d.insert("PairRecordID".into(), serial.as_str().into());
                "ReadPairRecord"
            }
        };
        d.insert("MessageType".into(), kind.into());
        d.insert("ClientVersionString".into(), "CellGlide-Desktop-1".into());
        d.insert("ProgName".into(), "CellGlideDesktop".into());
        d.insert("kLibUSBMuxVersion".into(), Value::Integer(3u64.into()));
        d
    }

    pub fn encode(&self, tag: u32) -> Result<Vec<u8>, ProtocolError> {
        encode_message(&self.fields(), tag)
    }
}

pub fn encode_message(dict: &Dictionary, tag: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(dict.clone()))?;
    let len = u32::try_from(body.len() + HEADER_LEN).map_err(|_| ProtocolError::TooLarge)?;
    if len > MAX_MESSAGE {
        return Err(ProtocolError::TooLarge);
    }
    let mut out = Vec::with_capacity(len as usize);
    for value in [len, VERSION, PLIST_TYPE, tag] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn body_len(header: &[u8; HEADER_LEN]) -> Result<usize, ProtocolError> {
    let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    let len = field(0);
    if len < HEADER_LEN as u32 || len > MAX_MESSAGE || field(1) != VERSION || field(2) != PLIST_TYPE {
        return Err(ProtocolError::Malformed("usbmux header"));
    }
    Ok(len as usize - HEADER_LEN)
}

pub fn decode_body(body: &[u8]) -> Result<Dictionary, ProtocolError> {
    Value::from_reader(Cursor::new(body))?
        .into_dictionary()
        .ok_or(ProtocolError::Malformed("usbmux plist is not a dictionary"))
}

fn uint(dict: &Dictionary, key: &str) -> Option<u64> {
    dict.get(key).and_then(Value::as_unsigned_integer)
}

pub fn result_number(dict: &Dictionary) -> Option<u64> {
    uint(dict, "Number")
}

/// A USB-connected device from a `DeviceList` item or an `Attached` event.
fn usb_device(item: &Dictionary) -> Option<Device> {
    let props = item.get("Properties")?.as_dictionary()?;
    if props.get("ConnectionType")?.as_string()? != "USB" {
        return None;
    }
    Some(Device {
        serial: props.get("SerialNumber")?.as_string()?.to_owned(),
        mux_id: u32::try_from(uint(item, "DeviceID")?).ok()?,
    })
}

pub fn device_list(dict: &Dictionary) -> Result<Vec<Device>, ProtocolError> {
    let list = dict
        .get("DeviceList")
        .and_then(Value::as_array)
        .ok_or(ProtocolError::Malformed("DeviceList missing"))?;
    let mut devices: Vec<Device> = list.iter().filter_map(Value::as_dictionary).filter_map(usb_device).collect();
    devices.sort_by(|a, b| a.serial.cmp(&b.serial));
    Ok(devices)
}

pub fn parse_event(dict: &Dictionary) -> MuxEvent {
    match dict.get("MessageType").and_then(Value::as_string) {
        Some("Attached") => usb_device(dict).map_or(MuxEvent::Other, MuxEvent::Attached),
        Some("Detached") => uint(dict, "DeviceID")
            .and_then(|id| u32::try_from(id).ok())
            .map_or(MuxEvent::Other, MuxEvent::Detached),
        _ => MuxEvent::Other,
    }
}

pub fn has_pair_record(dict: &Dictionary) -> bool {
    dict.contains_key("PairRecordData")
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p cg-protocol mux`
Expected: 5 个测试全部 `ok`。

- [ ] **Step 5: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-protocol && git commit -m "cg-protocol: usbmuxd plist messages

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: 手机端口消息（`cg-protocol::phone`）

参照 `CellProxy:Sources/DesktopCore/ConnectionPolicy.swift`（`ServiceHello`）、`HostIdentity.swift`（`watchHeaders`）、`USBSessionMonitor.swift`、`USBStatusMonitor.swift`、`CellProxy:Sources/SharedControl/SharingStatus.swift`、`PhoneMetrics.swift`、`CellProxy:Desktop/macOS/CellProxyMac/App/DesktopModel.swift` 的 `readHello`。

**Files:**
- Create: `crates/cg-protocol/src/phone.rs`
- Modify: `crates/cg-protocol/src/lib.rs`（加 `pub mod phone;`）

**Interfaces:**
- Produces:
  - `pub mod port { pub const SOCKS: u16 = 9876; pub const CONTROL: u16 = 8088; pub const STATUS: u16 = 8089; pub const METRICS: u16 = 8090; }`
  - `pub mod capability { SOCKS5_TCP, UDP_OVER_USB, DNS_FORWARD, SESSION_WATCH }`（`&str` 常量）
  - `pub struct Hello { service, protocol_version: i64, version, session_id, capabilities: Vec<String>, dns: Option<HelloDns> }`，方法 `validate()`、`has(&str) -> bool`、`dns_enabled() -> bool`
  - `pub const HELLO_REQUEST: &[u8]`；`pub const MAX_HTTP_HEAD: usize = 8192;`
  - `pub struct HttpHead { pub status: u16, pub content_length: Option<usize> }`；`pub fn parse_http_head(head: &[u8]) -> Result<HttpHead, ProtocolError>`
  - `pub fn watch_request(session_id: &str, computer_name: Option<&str>, link_speed: Option<u64>) -> String`
  - `pub struct SharingStatus { .. }` + `is_valid()`；`pub struct StatusSequence` + `accept(&mut self, &SharingStatus) -> Result<(), ProtocolError>`；`pub const MAX_STATUS_LINE: usize = 4096;`
  - `pub struct PhoneMetrics { .. }`（全部 `Option`）+ `is_valid()`；`pub const MAX_METRICS_LINE: usize = 4096;`

- [ ] **Step 1: 写失败的测试**

`phone.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";

    fn hello_json(caps: &str) -> String {
        format!(
            r#"{{"service":"CellProxy","protocolVersion":1,"version":"2.2.0","sessionID":"{SESSION}","capabilities":[{caps}],"dns":{{"enabled":false,"endpoint":"https://1.1.1.1/dns-query","cacheEnabled":true}}}}"#
        )
    }

    #[test]
    fn hello_parses_and_validates() {
        let hello: Hello = serde_json::from_str(&hello_json(r#""socks5-tcp","udp-over-usb-v1""#)).unwrap();
        hello.validate().unwrap();
        assert_eq!(hello.version, "2.2.0");
        assert!(hello.has(capability::UDP_OVER_USB));
        assert!(!hello.has(capability::SESSION_WATCH));
        assert!(!hello.dns_enabled());
    }

    #[test]
    fn hello_rejects_incompatible_phone() {
        let no_tcp: Hello = serde_json::from_str(&hello_json(r#""socks5-udp""#)).unwrap();
        assert!(matches!(no_tcp.validate(), Err(ProtocolError::Incompatible)));
        let bad_session: Hello = serde_json::from_str(
            r#"{"service":"CellProxy","protocolVersion":1,"version":"1","sessionID":"nope","capabilities":["socks5-tcp"]}"#,
        )
        .unwrap();
        assert!(bad_session.validate().is_err());
        assert!(bad_session.dns_enabled(), "missing dns block means enabled");
        let v2: Hello = serde_json::from_str(&hello_json(r#""socks5-tcp""#).replace(r#""protocolVersion":1"#, r#""protocolVersion":2"#)).unwrap();
        assert!(v2.validate().is_err());
    }

    #[test]
    fn http_head() {
        let head = parse_http_head(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 42\r\n\r\n").unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, Some(42));
        assert_eq!(parse_http_head(b"HTTP/1.1 404 Not Found\r\n\r\n").unwrap().status, 404);
        assert!(parse_http_head(b"SSH-2.0\r\n\r\n").is_err());
    }

    #[test]
    fn watch_request_encodes_name_and_speed() {
        let text = watch_request(SESSION, Some("Alice’s 电脑 ThinkPad"), Some(480_000_000));
        assert!(text.starts_with(&format!("GET /cellproxy/watch?session={SESSION} HTTP/1.1\r\nHost: localhost\r\n")));
        assert!(text.contains("X-CellGlide-Mac: Alice%E2%80%99s%20%E7%94%B5%E8%84%91%20ThinkPad\r\n"));
        assert!(text.contains("X-CellGlide-Link: usb; speed=480000000\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        let plain = watch_request(SESSION, None, None);
        assert_eq!(plain, format!("GET /cellproxy/watch?session={SESSION} HTTP/1.1\r\nHost: localhost\r\n\r\n"));
        let long = "x".repeat(100);
        assert!(watch_request(SESSION, Some(&long), None).contains(&format!("X-CellGlide-Mac: {}\r\n", "x".repeat(64))));
    }

    fn status(instance: &str, revision: u64) -> SharingStatus {
        serde_json::from_str(&format!(
            r#"{{"service":"CellGlide-status","version":1,"instance":"{instance}","revision":{revision},"running":true,"starting":false,"background":true,"dnsEnabled":true}}"#
        ))
        .unwrap()
    }

    #[test]
    fn status_sequence_requires_same_instance_and_growing_revision() {
        let mut seq = StatusSequence::default();
        seq.accept(&status(SESSION, 3)).unwrap();
        seq.accept(&status(SESSION, 4)).unwrap();
        assert!(seq.accept(&status(SESSION, 4)).is_err());
        let other = "7F9619FF-8B86-D011-B42D-00C04FC964FF";
        assert!(StatusSequence::default().accept(&status("not-a-uuid", 1)).is_err());
        let mut seq = StatusSequence::default();
        seq.accept(&status(SESSION, 1)).unwrap();
        assert!(seq.accept(&status(other, 9)).is_err());
    }

    #[test]
    fn metrics_accept_partial_frames() {
        let m: PhoneMetrics = serde_json::from_str(
            r#"{"service":"CellGlide-metrics","version":1,"carrier":"中国移动","radio":"5G SA","download":1024.5,"battery":0.8,"exitOK":true,"connectsFailed":{"timeout":2},"history":[[1,2],[3,4]]}"#,
        )
        .unwrap();
        assert!(m.is_valid());
        assert_eq!(m.carrier.as_deref(), Some("中国移动"));
        assert_eq!(m.exit_ok, Some(true));
        assert_eq!(m.upload, None);
        assert_eq!(m.connects_failed.unwrap()["timeout"], 2);
        let unknown: PhoneMetrics = serde_json::from_str(r#"{"service":"other","version":1}"#).unwrap();
        assert!(!unknown.is_valid());
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p cg-protocol phone`
Expected: 编译失败（类型未定义）。

- [ ] **Step 3: 实现**

`phone.rs` 顶部：

```rust
//! Messages on the iPhone app's loopback ports, reached through usbmuxd.

use crate::ProtocolError;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;

pub mod port {
    pub const SOCKS: u16 = 9876;
    pub const CONTROL: u16 = 8088;
    pub const STATUS: u16 = 8089;
    pub const METRICS: u16 = 8090;
}

pub mod capability {
    pub const SOCKS5_TCP: &str = "socks5-tcp";
    pub const UDP_OVER_USB: &str = "udp-over-usb-v1";
    pub const DNS_FORWARD: &str = "dns-forward-v1";
    pub const SESSION_WATCH: &str = "session-watch-v1";
}

pub const HELLO_REQUEST: &[u8] = b"GET /cellproxy/hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
pub const MAX_HTTP_HEAD: usize = 8192;
pub const MAX_HELLO_BODY: usize = 8192;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub service: String,
    pub protocol_version: i64,
    pub version: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub capabilities: Vec<String>,
    pub dns: Option<HelloDns>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HelloDns {
    pub enabled: Option<bool>,
    pub endpoint: String,
    pub cache_enabled: bool,
}

impl Hello {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let ok = self.service == "CellProxy"
            && self.protocol_version == 1
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.has(capability::SOCKS5_TCP);
        if ok { Ok(()) } else { Err(ProtocolError::Incompatible) }
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// The phone's "CellGlide DNS" switch; phones that send no dns block have it on.
    pub fn dns_enabled(&self) -> bool {
        self.dns.as_ref().and_then(|d| d.enabled).unwrap_or(true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpHead {
    pub status: u16,
    pub content_length: Option<usize>,
}

/// Parses a response head that ends with CRLF CRLF.
pub fn parse_http_head(head: &[u8]) -> Result<HttpHead, ProtocolError> {
    let text = std::str::from_utf8(head).map_err(|_| ProtocolError::Malformed("http head"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or(ProtocolError::Malformed("http status"))?;
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok());
    Ok(HttpHead { status, content_length })
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The session-watch request. The phone shows the computer's name and link from these
/// headers; both are bounded and percent-encoded so they cannot break the framing.
pub fn watch_request(session_id: &str, computer_name: Option<&str>, link_speed: Option<u64>) -> String {
    let mut text = format!("GET /cellproxy/watch?session={session_id} HTTP/1.1\r\nHost: localhost\r\n");
    if let Some(name) = computer_name.filter(|n| !n.is_empty()) {
        let short: String = name.chars().take(64).collect();
        let _ = write!(text, "X-CellGlide-Mac: {}\r\n", percent_encode(&short));
    }
    if let Some(speed) = link_speed {
        let _ = write!(text, "X-CellGlide-Link: usb; speed={speed}\r\n");
    }
    text.push_str("\r\n");
    text
}

pub const MAX_STATUS_LINE: usize = 4096;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharingStatus {
    pub service: String,
    pub version: i64,
    pub instance: String,
    pub revision: u64,
    pub running: bool,
    pub starting: bool,
    pub background: bool,
    pub dns_enabled: bool,
}

impl SharingStatus {
    pub fn is_valid(&self) -> bool {
        self.service == "CellGlide-status" && self.version == 1 && uuid::Uuid::parse_str(&self.instance).is_ok()
    }
}

/// Status frames must come from one phone instance with growing revisions.
#[derive(Debug, Default)]
pub struct StatusSequence {
    last: Option<(String, u64)>,
}

impl StatusSequence {
    pub fn accept(&mut self, status: &SharingStatus) -> Result<(), ProtocolError> {
        let in_order = match &self.last {
            None => true,
            Some((instance, revision)) => *instance == status.instance && status.revision > *revision,
        };
        if !status.is_valid() || !in_order {
            return Err(ProtocolError::Malformed("status sequence"));
        }
        self.last = Some((status.instance.clone(), status.revision));
        Ok(())
    }
}

pub const MAX_METRICS_LINE: usize = 4096;

/// One frame per second from the phone; every reading is optional.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PhoneMetrics {
    pub service: String,
    pub version: i64,
    pub carrier: Option<String>,
    pub radio: Option<String>,
    pub sharing: Option<bool>,
    /// Bytes per second the computer moves through the phone.
    pub download: Option<f64>,
    pub upload: Option<f64>,
    pub session_bytes: Option<i64>,
    pub quality_score: Option<i64>,
    pub latency: Option<f64>,
    pub loss: Option<f64>,
    /// 0–1.
    pub battery: Option<f64>,
    pub charging: Option<bool>,
    pub low_power: Option<bool>,
    pub thermal: Option<i64>,
    #[serde(rename = "exitOK")]
    pub exit_ok: Option<bool>,
    pub exit_latency: Option<f64>,
    #[serde(rename = "connectsOK")]
    pub connects_ok: Option<i64>,
    pub connects_failed: Option<HashMap<String, i64>>,
    pub history: Option<Vec<Vec<i64>>>,
}

impl PhoneMetrics {
    pub fn is_valid(&self) -> bool {
        self.service == "CellGlide-metrics" && self.version == 1
    }
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p cg-protocol phone`
Expected: 6 个测试 `ok`。

- [ ] **Step 5: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-protocol && git commit -m "cg-protocol: phone hello, watch, status and metrics messages

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: SOCKS5 片段与 UDP-over-USB 帧（`cg-protocol::socks`）

参照 `CellProxy:Docs/USB-UDP-v1.md` 和 `USBTransport.swift` 的 `negotiateUDP` / `associateUDP`。

**Files:**
- Create: `crates/cg-protocol/src/socks.rs`
- Modify: `crates/cg-protocol/src/lib.rs`（加 `pub mod socks;`）

**Interfaces:**
- Produces:
  - `pub const UDP_OVER_USB_REQUEST: [u8; 10]`（`[5, 0xf0, 0, 1, 0,0,0,0, 0,0]`）
  - `pub const REPLY_COMMAND_FAILED: [u8; 10]`（`[5, 7, 0, 1, 0,0,0,0, 0,0]`）
  - `pub fn udp_associate_reply(port: u16) -> [u8; 10]`（绑定 `127.0.0.1:port`）
  - `pub const MAX_FRAME: usize = 65535;`；`pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, ProtocolError>`
  - `pub fn address_rest_len(atyp: u8, first: u8) -> Result<usize, ProtocolError>`：读到 ATYP 与其后第一个字节后，还需读多少字节（含端口）
  - `pub fn connect_domain_request(host: &str, port: u16) -> Result<Vec<u8>, ProtocolError>`（出口检测用）

- [ ] **Step 1: 写失败的测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_big_endian_length() {
        assert_eq!(encode_frame(&[]).unwrap(), vec![0, 0]);
        let frame = encode_frame(&[7; 300]).unwrap();
        assert_eq!(&frame[..2], &[1, 44]);
        assert_eq!(frame.len(), 302);
        assert!(encode_frame(&vec![0; MAX_FRAME]).is_ok());
        assert!(encode_frame(&vec![0; MAX_FRAME + 1]).is_err());
    }

    #[test]
    fn address_lengths_after_first_byte() {
        // IPv4: first byte is part of the 4-byte address, then 3 + 2 more.
        assert_eq!(address_rest_len(1, 10).unwrap(), 5);
        // Domain: first byte is the length.
        assert_eq!(address_rest_len(3, 11).unwrap(), 13);
        assert_eq!(address_rest_len(4, 0).unwrap(), 17);
        assert!(address_rest_len(9, 0).is_err());
    }

    #[test]
    fn replies_and_requests() {
        assert_eq!(udp_associate_reply(0x1234), [5, 0, 0, 1, 127, 0, 0, 1, 0x12, 0x34]);
        assert_eq!(UDP_OVER_USB_REQUEST[1], 0xf0);
        assert_eq!(
            connect_domain_request("a.io", 443).unwrap(),
            vec![5, 1, 0, 3, 4, b'a', b'.', b'i', b'o', 1, 187]
        );
        assert!(connect_domain_request(&"x".repeat(256), 1).is_err());
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p cg-protocol socks`
Expected: 编译失败。

- [ ] **Step 3: 实现**

```rust
//! SOCKS5 pieces the forwarder needs, and the phone's udp-over-usb-v1 framing:
//! after the private command 0xF0 each frame is a big-endian u16 length followed by
//! one SOCKS5 UDP packet (RSV, FRAG, address, payload).

use crate::ProtocolError;

pub const UDP_OVER_USB_REQUEST: [u8; 10] = [5, 0xf0, 0, 1, 0, 0, 0, 0, 0, 0];
pub const REPLY_COMMAND_FAILED: [u8; 10] = [5, 7, 0, 1, 0, 0, 0, 0, 0, 0];
pub const MAX_FRAME: usize = 65535;

pub fn udp_associate_reply(port: u16) -> [u8; 10] {
    let [hi, lo] = port.to_be_bytes();
    [5, 0, 0, 1, 127, 0, 0, 1, hi, lo]
}

pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let len = u16::try_from(payload.len()).map_err(|_| ProtocolError::TooLarge)?;
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Bytes still to read (port included) once ATYP and the byte after it are known.
pub fn address_rest_len(atyp: u8, first: u8) -> Result<usize, ProtocolError> {
    match atyp {
        1 => Ok(3 + 2),
        3 => Ok(first as usize + 2),
        4 => Ok(15 + 2),
        _ => Err(ProtocolError::Malformed("socks address type")),
    }
}

pub fn connect_domain_request(host: &str, port: u16) -> Result<Vec<u8>, ProtocolError> {
    let len = u8::try_from(host.len()).map_err(|_| ProtocolError::TooLarge)?;
    let mut out = vec![5, 1, 0, 3, len];
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p cg-protocol`
Expected: cg-protocol 全部测试 `ok`。

- [ ] **Step 5: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-protocol && git commit -m "cg-protocol: SOCKS5 pieces and udp-over-usb-v1 frames

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6: 控制通道消息与客户端（`cg-ipc`）

服务、命令行、界面三方共用的消息类型。状态快照（`Snapshot`）也定义在这里，`cg-core` 依赖它。

**Files:**
- Create: `crates/cg-ipc/Cargo.toml`, `crates/cg-ipc/src/lib.rs`, `crates/cg-ipc/src/client.rs`

**Interfaces:**
- Produces:
  - 常量 `SOCKET_PATH = "/run/cellglide/control.sock"`、`GROUP = "cellglide"`、`MAX_LINE = 65536`
  - `pub enum ConnState { NoUsbService, #[default] NoPhone, ChoosePhone, Idle, Connecting, Connected { phone_version: String, phone_dns: bool }, Retrying { reason: String } }`（serde：`tag = "state"`、snake_case）
  - `pub struct MetricsSummary { carrier, radio: Option<String>, download, upload, battery: Option<f64>, charging: Option<bool> }`
  - `pub struct Snapshot { state: ConnState, devices: Vec<String>, selected: Option<String>, wants_connection: bool, metrics: Option<MetricsSummary> }`（`Default`）
  - `pub enum Request { Status, Connect { serial: Option<String> }, Disconnect, Doctor, Subscribe }`（`tag = "cmd"`）
  - `pub enum Response { Status { snapshot: Snapshot }, Ok, Error { message: String }, Doctor { checks: Vec<Check> } }`（`tag = "type"`）
  - `pub struct Check { name: String, ok: bool, detail: String, fix: Option<String> }`
  - `pub fn to_line<T: Serialize>(&T) -> Vec<u8>`、`pub fn from_line<T: DeserializeOwned>(&[u8]) -> serde_json::Result<T>`
  - `pub async fn read_line<R: AsyncBufRead + Unpin>(&mut R) -> io::Result<Option<Vec<u8>>>`（EOF 返回 `None`；超长返回 `InvalidData`）
  - `client::request(path: &Path, req: &Request) -> Result<Response, ClientError>`、`client::subscribe(path: &Path) -> Result<mpsc::Receiver<Snapshot>, ClientError>`（仅 unix）

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "cg-ipc"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: 写失败的测试**

`crates/cg-ipc/src/lib.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes_are_stable() {
        assert_eq!(to_line(&Request::Connect { serial: None }), b"{\"cmd\":\"connect\",\"serial\":null}\n");
        assert_eq!(to_line(&Request::Status), b"{\"cmd\":\"status\"}\n");
        let snap = Snapshot {
            state: ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: true },
            devices: vec!["abc".into()],
            ..Snapshot::default()
        };
        let text = String::from_utf8(to_line(&Response::Status { snapshot: snap.clone() })).unwrap();
        assert!(text.contains("\"type\":\"status\""));
        assert!(text.contains("\"state\":\"connected\""));
        let back: Response = from_line(text.as_bytes()).unwrap();
        assert_eq!(back, Response::Status { snapshot: snap });
        assert_eq!(Snapshot::default().state, ConnState::NoPhone);
    }

    #[tokio::test]
    async fn read_line_limits_length() {
        let mut ok = tokio::io::BufReader::new(&b"{\"cmd\":\"status\"}\nrest"[..]);
        assert_eq!(read_line(&mut ok).await.unwrap().unwrap(), b"{\"cmd\":\"status\"}");
        let mut eof = tokio::io::BufReader::new(&b""[..]);
        assert!(read_line(&mut eof).await.unwrap().is_none());
        let long = vec![b'x'; MAX_LINE + 10];
        let mut too_long = tokio::io::BufReader::new(&long[..]);
        assert_eq!(read_line(&mut too_long).await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }
}
```

`crates/cg-ipc/src/client.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn request_and_subscribe_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut r = BufReader::new(r);
                    while let Some(line) = crate::read_line(&mut r).await.unwrap() {
                        match crate::from_line::<Request>(&line).unwrap() {
                            Request::Subscribe => {
                                for _ in 0..2 {
                                    let snap = Snapshot::default();
                                    w.write_all(&crate::to_line(&Response::Status { snapshot: snap })).await.unwrap();
                                }
                            }
                            _ => w.write_all(&crate::to_line(&Response::Ok)).await.unwrap(),
                        }
                    }
                });
            }
        });
        assert_eq!(request(&path, &Request::Disconnect).await.unwrap(), Response::Ok);
        let mut rx = subscribe(&path).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
        assert_eq!(rx.recv().await.unwrap(), Snapshot::default());
    }

    #[tokio::test]
    async fn missing_socket_is_an_io_error() {
        let err = request(Path::new("/nonexistent/cellglide.sock"), &Request::Status).await.unwrap_err();
        assert!(matches!(err, ClientError::Io(_)));
    }
}
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-ipc`
Expected: 编译失败。

- [ ] **Step 4: 实现 `lib.rs`**

```rust
//! Messages between cellglide-service and its clients: one JSON object per line.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

#[cfg(unix)]
pub mod client;

pub const SOCKET_PATH: &str = "/run/cellglide/control.sock";
pub const GROUP: &str = "cellglide";
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    /// usbmuxd is not reachable.
    NoUsbService,
    #[default]
    NoPhone,
    /// More than one iPhone is attached and none was chosen.
    ChoosePhone,
    /// A phone is attached; nobody asked to connect.
    Idle,
    Connecting,
    Connected { phone_version: String, phone_dns: bool },
    /// Asked to connect; the last attempt failed and will be retried.
    Retrying { reason: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub carrier: Option<String>,
    pub radio: Option<String>,
    /// Bytes per second through the phone.
    pub download: Option<f64>,
    pub upload: Option<f64>,
    /// 0–1.
    pub battery: Option<f64>,
    pub charging: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub state: ConnState,
    /// Serials of attached iPhones.
    pub devices: Vec<String>,
    pub selected: Option<String>,
    pub wants_connection: bool,
    pub metrics: Option<MetricsSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Connect { serial: Option<String> },
    Disconnect,
    Doctor,
    Subscribe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Status { snapshot: Snapshot },
    Ok,
    Error { message: String },
    Doctor { checks: Vec<Check> },
}

pub fn to_line<T: Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("IPC types always serialize");
    bytes.push(b'\n');
    bytes
}

pub fn from_line<T: DeserializeOwned>(line: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(line)
}

/// One line without its newline; `None` at a clean end of stream.
pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = reader.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long or truncated"));
    }
    line.pop();
    Ok(Some(line))
}
```

- [ ] **Step 5: 实现 `client.rs`**

```rust
//! Client side of the control socket, used by cellglide and cellglide-gui.

use crate::{Request, Response, Snapshot, from_line, read_line, to_line};
use std::path::Path;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("连不上 CellGlide 服务：{0}")]
    Io(#[from] std::io::Error),
    #[error("服务回复无法解析：{0}")]
    Json(#[from] serde_json::Error),
    #[error("服务关闭了连接")]
    Closed,
}

pub async fn request(path: &Path, request: &Request) -> Result<Response, ClientError> {
    let stream = UnixStream::connect(path).await?;
    let (r, mut w) = stream.into_split();
    w.write_all(&to_line(request)).await?;
    let line = read_line(&mut BufReader::new(r)).await?.ok_or(ClientError::Closed)?;
    Ok(from_line(&line)?)
}

/// Snapshots as the service publishes them, starting with the current one.
/// The receiver closes when the service goes away.
pub async fn subscribe(path: &Path) -> Result<mpsc::Receiver<Snapshot>, ClientError> {
    let stream = UnixStream::connect(path).await?;
    let (r, mut w) = stream.into_split();
    w.write_all(&to_line(&Request::Subscribe)).await?;
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _keep_open = w;
        let mut r = BufReader::new(r);
        while let Ok(Some(line)) = read_line(&mut r).await {
            let Ok(Response::Status { snapshot }) = from_line::<Response>(&line) else { break };
            if tx.send(snapshot).await.is_err() {
                break;
            }
        }
    });
    Ok(rx)
}
```

- [ ] **Step 6: 运行，确认通过**

Run: `cargo test -p cg-ipc`
Expected: 4 个测试 `ok`。

- [ ] **Step 7: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-ipc && git commit -m "cg-ipc: control socket messages and client

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: usbmuxd 异步客户端与假 usbmuxd（`cg-core::mux`、`cg-core::testing::fake_mux`）

**Files:**
- Create: `crates/cg-core/Cargo.toml`, `crates/cg-core/src/lib.rs`, `crates/cg-core/src/error.rs`, `crates/cg-core/src/mux.rs`, `crates/cg-core/src/testing/mod.rs`, `crates/cg-core/src/testing/fake_mux.rs`

**Interfaces:**
- Consumes: `cg_protocol::mux::*`（Task 3）
- Produces:
  - `cg_core::CoreError`（见下方代码，含中文提示）、`cg_core::Result<T>`
  - `pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}`、`pub type MuxStream = Box<dyn Stream>`
  - `pub enum MuxEndpoint { Unix(PathBuf) /* unix */, Tcp(SocketAddr) }`，`fn system() -> Self`，`async fn open(&self) -> Result<MuxStream>`
  - `mux::list_devices(&MuxEndpoint) -> Result<Vec<Device>>`、`mux::connect(&MuxEndpoint, mux_id: u32, port: u16) -> Result<MuxStream>`、`mux::has_pair_record(&MuxEndpoint, &str) -> Result<bool>`、`mux::listen(&MuxEndpoint) -> Result<mpsc::Receiver<MuxEvent>>`
  - 测试工具（`#[cfg(any(test, feature = "testing"))]`）：`testing::FakeMux`（`start()`、`endpoint()`、`attach(Device)`、`detach(u32)`、`route(u16, SocketAddr)`、`add_pair_record(&str)`、`drop_listeners()`）、`testing::echo_server() -> SocketAddr`、`testing::device(serial: &str, id: u32) -> Device`

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "cg-core"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[features]
testing = ["dep:tempfile"]

[dependencies]
bytes.workspace = true
cg-ipc.workspace = true
cg-protocol.workspace = true
http-body-util.workspace = true
hyper.workspace = true
hyper-util.workspace = true
plist.workspace = true
serde_json.workspace = true
tempfile = { workspace = true, optional = true }
thiserror.workspace = true
tokio.workspace = true
tokio-util.workspace = true
tracing.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: `lib.rs` 与 `error.rs`**

`crates/cg-core/src/lib.rs`：

```rust
//! Platform-neutral core: talks to usbmuxd and the phone, relays traffic,
//! answers DNS, and drives the connection state machine.

mod error;
pub mod mux;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::{CoreError, Result};
```

`crates/cg-core/src/error.rs`：

```rust
use cg_protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Protocol(#[from] ProtocolError),
    #[error("等待手机响应超时")]
    Timeout,
    #[error("找不到 USB 服务（usbmuxd），请先安装并启动 usbmuxd")]
    NoUsbService,
    #[error("手机服务未就绪，请解锁、信任此电脑，并打开 CellGlide 开始共享")]
    PhoneNotReady,
    #[error("请更新手机上的 CellGlide，当前版本太旧")]
    PhoneTooOld,
    #[error("手机上的共享会话已变化")]
    SessionChanged,
    #[error("手机已连上，但手机网络暂时不通。请检查手机信号，并确认手机上的 CellGlide 正在共享。")]
    NoExit,
    #[error("所选手机已断开")]
    PhoneGone,
    #[error("本机 DNS 端口 {0} 已被占用")]
    ResolverPortBusy(u16),
    #[error("{0}")]
    Tunnel(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
```

- [ ] **Step 3: 写假 usbmuxd**

`crates/cg-core/src/testing/mod.rs`：

```rust
//! In-process stand-ins for usbmuxd, the phone app and the tunnel backend.

mod fake_mux;

pub use fake_mux::FakeMux;

use cg_protocol::mux::Device;
use std::net::SocketAddr;
use tokio::net::TcpListener;

pub fn device(serial: &str, mux_id: u32) -> Device {
    Device { serial: serial.into(), mux_id }
}

/// A TCP server that echoes every connection.
pub async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}
```

`crates/cg-core/src/testing/fake_mux.rs`：

```rust
use crate::mux::{MuxEndpoint, read_message};
use cg_protocol::mux::{Device, MuxEvent, encode_message};
use plist::{Dictionary, Value};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Default)]
struct State {
    devices: Vec<Device>,
    routes: HashMap<u16, SocketAddr>,
    pair_records: HashSet<String>,
    listeners: Vec<mpsc::UnboundedSender<MuxEvent>>,
}

/// A usbmuxd on a temporary Unix socket. `Connect` to a routed port is piped to a local TCP server.
pub struct FakeMux {
    _dir: tempfile::TempDir,
    path: PathBuf,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeMux {
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                tokio::spawn(serve(conn, shared.clone()));
            }
        });
        Self { _dir: dir, path, state, task }
    }

    pub fn endpoint(&self) -> MuxEndpoint {
        MuxEndpoint::Unix(self.path.clone())
    }

    pub fn attach(&self, device: Device) {
        let mut s = self.state.lock().unwrap();
        s.devices.push(device.clone());
        s.listeners.retain(|l| l.send(MuxEvent::Attached(device.clone())).is_ok());
    }

    pub fn detach(&self, mux_id: u32) {
        let mut s = self.state.lock().unwrap();
        s.devices.retain(|d| d.mux_id != mux_id);
        s.listeners.retain(|l| l.send(MuxEvent::Detached(mux_id)).is_ok());
    }

    pub fn route(&self, port: u16, to: SocketAddr) {
        self.state.lock().unwrap().routes.insert(port, to);
    }

    pub fn add_pair_record(&self, serial: &str) {
        self.state.lock().unwrap().pair_records.insert(serial.into());
    }

    /// Closes every Listen connection, as when usbmuxd restarts.
    pub fn drop_listeners(&self) {
        self.state.lock().unwrap().listeners.clear();
    }
}

impl Drop for FakeMux {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn result(number: u64) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), "Result".into());
    d.insert("Number".into(), Value::Integer(number.into()));
    d
}

fn device_dict(device: &Device) -> Dictionary {
    let mut props = Dictionary::new();
    props.insert("ConnectionType".into(), "USB".into());
    props.insert("SerialNumber".into(), device.serial.as_str().into());
    let mut d = Dictionary::new();
    d.insert("DeviceID".into(), Value::Integer(u64::from(device.mux_id).into()));
    d.insert("Properties".into(), Value::Dictionary(props));
    d
}

async fn send(conn: &mut UnixStream, dict: &Dictionary) -> std::io::Result<()> {
    conn.write_all(&encode_message(dict, 1).unwrap()).await
}

async fn serve(mut conn: UnixStream, state: Arc<Mutex<State>>) {
    let Ok(request) = read_message(&mut conn).await else { return };
    let uint = |key: &str| request.get(key).and_then(Value::as_unsigned_integer);
    match request.get("MessageType").and_then(Value::as_string) {
        Some("ListDevices") => {
            let list = state.lock().unwrap().devices.iter().map(|d| Value::Dictionary(device_dict(d))).collect();
            let mut reply = Dictionary::new();
            reply.insert("DeviceList".into(), Value::Array(list));
            let _ = send(&mut conn, &reply).await;
        }
        Some("Listen") => {
            let (tx, mut rx) = mpsc::unbounded_channel();
            state.lock().unwrap().listeners.push(tx);
            if send(&mut conn, &result(0)).await.is_err() {
                return;
            }
            while let Some(event) = rx.recv().await {
                let message = match event {
                    MuxEvent::Attached(d) => {
                        let mut m = device_dict(&d);
                        m.insert("MessageType".into(), "Attached".into());
                        m
                    }
                    MuxEvent::Detached(id) => {
                        let mut m = Dictionary::new();
                        m.insert("MessageType".into(), "Detached".into());
                        m.insert("DeviceID".into(), Value::Integer(u64::from(id).into()));
                        m
                    }
                    MuxEvent::Other => continue,
                };
                if send(&mut conn, &message).await.is_err() {
                    return;
                }
            }
        }
        Some("Connect") => {
            let id = uint("DeviceID").unwrap_or(u64::MAX);
            let wire = uint("PortNumber").unwrap_or(0) as u16;
            let port = u16::from_be_bytes(wire.to_le_bytes());
            let target = {
                let s = state.lock().unwrap();
                s.devices.iter().any(|d| u64::from(d.mux_id) == id).then(|| s.routes.get(&port).copied()).flatten()
            };
            let upstream = match target {
                Some(addr) => TcpStream::connect(addr).await.ok(),
                None => None,
            };
            let Some(mut upstream) = upstream else {
                let _ = send(&mut conn, &result(3)).await;
                return;
            };
            if send(&mut conn, &result(0)).await.is_ok() {
                let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
            }
        }
        Some("ReadPairRecord") => {
            let serial = request.get("PairRecordID").and_then(Value::as_string).unwrap_or_default();
            let reply = if state.lock().unwrap().pair_records.contains(serial) {
                let mut d = Dictionary::new();
                d.insert("PairRecordData".into(), Value::Data(vec![1]));
                d
            } else {
                result(2)
            };
            let _ = send(&mut conn, &reply).await;
        }
        _ => {
            let _ = send(&mut conn, &result(1)).await;
        }
    }
}
```

- [ ] **Step 4: 写失败的测试**

`crates/cg-core/src/mux.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, device, echo_server};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn lists_and_connects() {
        let mux = FakeMux::start().await;
        mux.attach(device("bbbb", 2));
        mux.attach(device("aaaa", 1));
        mux.route(9876, echo_server().await);
        let ep = mux.endpoint();
        let devices = list_devices(&ep).await.unwrap();
        assert_eq!(devices.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>(), ["aaaa", "bbbb"]);

        let mut stream = connect(&ep, 1, 9876).await.unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        assert!(matches!(connect(&ep, 1, 1234).await, Err(CoreError::PhoneNotReady)));
        assert!(matches!(connect(&ep, 99, 9876).await, Err(CoreError::PhoneNotReady)));
    }

    #[tokio::test]
    async fn listen_reports_attach_detach_and_closes_on_restart() {
        let mux = FakeMux::start().await;
        let mut events = listen(&mux.endpoint()).await.unwrap();
        mux.attach(device("aaaa", 1));
        assert_eq!(events.recv().await, Some(MuxEvent::Attached(device("aaaa", 1))));
        mux.detach(1);
        assert_eq!(events.recv().await, Some(MuxEvent::Detached(1)));
        mux.drop_listeners();
        assert_eq!(events.recv().await, None);
    }

    #[tokio::test]
    async fn missing_usbmuxd_and_pair_records() {
        let gone = MuxEndpoint::Unix("/nonexistent/usbmuxd".into());
        assert!(matches!(list_devices(&gone).await, Err(CoreError::NoUsbService)));
        let mux = FakeMux::start().await;
        assert!(!has_pair_record(&mux.endpoint(), "aaaa").await.unwrap());
        mux.add_pair_record("aaaa");
        assert!(has_pair_record(&mux.endpoint(), "aaaa").await.unwrap());
    }
}
```

`lib.rs` 已有 `pub mod mux;`。

- [ ] **Step 5: 运行，确认失败**

Run: `cargo test -p cg-core mux`
Expected: 编译失败（`list_devices` 等未定义）。

- [ ] **Step 6: 实现 `mux.rs`**

```rust
//! usbmuxd client: list devices, follow attach/detach, and open a TCP channel to a
//! port on the phone. On Linux and macOS usbmuxd is a Unix socket; on Windows,
//! Apple Mobile Device Service listens on TCP 127.0.0.1:27015.

use crate::{CoreError, Result};
use cg_protocol::mux::{self, Device, MuxEvent, MuxRequest};
use plist::Dictionary;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type MuxStream = Box<dyn Stream>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxEndpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(SocketAddr),
}

fn open_error(error: std::io::Error) -> CoreError {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    match error.kind() {
        NotFound | ConnectionRefused => CoreError::NoUsbService,
        _ => error.into(),
    }
}

impl MuxEndpoint {
    pub fn system() -> Self {
        #[cfg(unix)]
        {
            Self::Unix(PathBuf::from("/var/run/usbmuxd"))
        }
        #[cfg(not(unix))]
        {
            Self::Tcp(SocketAddr::from(([127, 0, 0, 1], 27015)))
        }
    }

    pub async fn open(&self) -> Result<MuxStream> {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path).await.map_err(open_error)?;
                Ok(Box::new(stream))
            }
            Self::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(addr).await.map_err(open_error)?;
                stream.set_nodelay(true)?;
                Ok(Box::new(stream))
            }
        }
    }
}

pub async fn read_message<S: AsyncRead + Unpin + ?Sized>(stream: &mut S) -> Result<Dictionary> {
    let mut header = [0u8; mux::HEADER_LEN];
    stream.read_exact(&mut header).await?;
    let mut body = vec![0u8; mux::body_len(&header)?];
    stream.read_exact(&mut body).await?;
    Ok(mux::decode_body(&body)?)
}

async fn exchange(stream: &mut MuxStream, request: &MuxRequest) -> Result<Dictionary> {
    let bytes = request.encode(1)?;
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        stream.write_all(&bytes).await?;
        read_message(stream.as_mut()).await
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}

async fn request(endpoint: &MuxEndpoint, request: &MuxRequest) -> Result<(MuxStream, Dictionary)> {
    let mut stream = endpoint.open().await?;
    let reply = exchange(&mut stream, request).await?;
    Ok((stream, reply))
}

pub async fn list_devices(endpoint: &MuxEndpoint) -> Result<Vec<Device>> {
    let (_, reply) = request(endpoint, &MuxRequest::ListDevices).await?;
    Ok(mux::device_list(&reply)?)
}

/// A byte stream to `port` on the phone. No timeouts stay on it.
pub async fn connect(endpoint: &MuxEndpoint, mux_id: u32, port: u16) -> Result<MuxStream> {
    let (stream, reply) = request(endpoint, &MuxRequest::Connect { mux_id, port }).await?;
    match mux::result_number(&reply) {
        Some(0) => Ok(stream),
        _ => Err(CoreError::PhoneNotReady),
    }
}

/// Whether this computer was trusted on the phone (usbmuxd holds a pairing record).
pub async fn has_pair_record(endpoint: &MuxEndpoint, serial: &str) -> Result<bool> {
    let (_, reply) = request(endpoint, &MuxRequest::ReadPairRecord { serial: serial.into() }).await?;
    Ok(mux::has_pair_record(&reply))
}

/// Attach and detach events; the receiver closes when usbmuxd goes away.
pub async fn listen(endpoint: &MuxEndpoint) -> Result<mpsc::Receiver<MuxEvent>> {
    let (mut stream, reply) = request(endpoint, &MuxRequest::Listen).await?;
    if mux::result_number(&reply) != Some(0) {
        return Err(cg_protocol::ProtocolError::Malformed("usbmuxd refused Listen").into());
    }
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        while let Ok(message) = read_message(stream.as_mut()).await {
            if tx.send(mux::parse_event(&message)).await.is_err() {
                break;
            }
        }
    });
    Ok(rx)
}
```

注意：`FakeMux` 里调用的是 `read_message(&mut conn)`，`conn: UnixStream` 满足 `AsyncRead + Unpin`，签名中的 `?Sized` 让 `stream.as_mut()`（`&mut dyn Stream`）也能用。

- [ ] **Step 7: 运行，确认通过（macOS 和 Docker 各跑一次）**

Run: `cargo test -p cg-core mux && scripts/linux-test.sh -p cg-core mux`
Expected: 两处都是 3 个测试 `ok`。

- [ ] **Step 8: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-core && git commit -m "cg-core: async usbmuxd client and fake usbmuxd

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: 手机会话（握手、watch、状态、指标）与假手机

参照 `CellProxy:Desktop/macOS/CellProxyMac/App/DesktopModel.swift` 的 `readHello`、`USBSessionMonitor.swift`、`USBStatusMonitor.swift`、`PhoneMetricsMonitor.swift`。

**Files:**
- Create: `crates/cg-core/src/phone.rs`, `crates/cg-core/src/testing/fake_phone.rs`
- Modify: `crates/cg-core/src/lib.rs`（加 `pub mod phone;`）、`crates/cg-core/src/testing/mod.rs`（加 `mod fake_phone; pub use fake_phone::{FakePhone, FakePhoneOptions, hello_json};`）

**Interfaces:**
- Consumes: `mux::connect`、`MuxEndpoint`、`MuxStream`（Task 7）；`cg_protocol::phone::*`（Task 4）
- Produces:
  - `#[derive(Clone, Debug)] pub struct Phone { pub endpoint: MuxEndpoint, pub device: Device }`，`async fn open(&self, port: u16) -> Result<MuxStream>`
  - `phone::read_hello(&Phone) -> Result<Hello>`
  - `phone::watch(&Phone, session_id: &str, computer_name: Option<&str>) -> Result<BoxFuture<'static, ()>>`：握手成功后返回一个 future，会话结束时完成
  - `phone::status_stream(Phone, mpsc::Sender<SharingStatus>) -> Result<()>`、`phone::metrics_stream(Phone, mpsc::Sender<PhoneMetrics>) -> Result<()>`（出错或通道关闭时返回）
  - `phone::METRICS_SILENCE: Duration = 5s`
  - 测试工具：`FakePhone::start(&FakeMux, FakePhoneOptions) -> FakePhone`；`FakePhone::{end_session, set_hello, send_status, send_metrics, doh_queries}`；`FakePhoneOptions { hello: serde_json::Value, exit_ok: bool, udp_ok: bool }`（`Default`）；`hello_json(caps: &[&str], dns_enabled: bool, session_id: &str) -> serde_json::Value`；常量 `testing::SESSION_A`、`testing::SESSION_B`

- [ ] **Step 1: 写假手机**

`crates/cg-core/src/testing/fake_phone.rs`：

```rust
use super::FakeMux;
use cg_protocol::phone::port;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};

pub const SESSION_A: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
pub const SESSION_B: &str = "7F9619FF-8B86-D011-B42D-00C04FC964FF";

pub fn hello_json(caps: &[&str], dns_enabled: bool, session_id: &str) -> Value {
    json!({
        "service": "CellProxy", "protocolVersion": 1, "version": "2.2.0",
        "sessionID": session_id, "capabilities": caps,
        "dns": {"enabled": dns_enabled, "endpoint": "https://1.1.1.1/dns-query", "cacheEnabled": true}
    })
}

pub struct FakePhoneOptions {
    pub hello: Value,
    /// Whether SOCKS CONNECT succeeds (the phone's own way out works).
    pub exit_ok: bool,
    /// Whether the phone accepts udp-over-usb-v1 (command 0xF0).
    pub udp_ok: bool,
}

impl Default for FakePhoneOptions {
    fn default() -> Self {
        Self {
            hello: hello_json(
                &["socks5-tcp", "socks5-udp", "udp-over-usb-v1", "http-connect", "dns-forward-v1", "session-watch-v1"],
                true,
                SESSION_A,
            ),
            exit_ok: true,
            udp_ok: true,
        }
    }
}

struct Shared {
    hello: Mutex<Value>,
    exit_ok: bool,
    udp_ok: bool,
    /// Bumped by `end_session`; watch connections close when it changes.
    generation: watch::Sender<u64>,
    statuses: broadcast::Sender<String>,
    metrics: broadcast::Sender<String>,
    doh_queries: AtomicUsize,
}

/// The phone app's loopback services behind a FakeMux.
pub struct FakePhone {
    shared: Arc<Shared>,
}

async fn serve_on(listener: TcpListener, shared: Arc<Shared>, handler: fn(TcpStream, Arc<Shared>) -> tokio::task::JoinHandle<()>) {
    while let Ok((stream, _)) = listener.accept().await {
        handler(stream, shared.clone());
    }
}

async fn bind() -> (TcpListener, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

impl FakePhone {
    pub async fn start(mux: &FakeMux, options: FakePhoneOptions) -> Self {
        let shared = Arc::new(Shared {
            hello: Mutex::new(options.hello),
            exit_ok: options.exit_ok,
            udp_ok: options.udp_ok,
            generation: watch::channel(0).0,
            statuses: broadcast::channel(16).0,
            metrics: broadcast::channel(16).0,
            doh_queries: AtomicUsize::new(0),
        });
        for (port, handler) in [
            (port::CONTROL, control as fn(TcpStream, Arc<Shared>) -> tokio::task::JoinHandle<()>),
            (port::SOCKS, socks),
            (port::STATUS, status_lines),
            (port::METRICS, metric_lines),
        ] {
            let (listener, addr) = bind().await;
            mux.route(port, addr);
            tokio::spawn(serve_on(listener, shared.clone(), handler));
        }
        Self { shared }
    }

    /// Ends every session watch, as when the user stops sharing on the phone.
    pub fn end_session(&self) {
        self.shared.generation.send_modify(|g| *g += 1);
    }

    pub fn set_hello(&self, hello: Value) {
        *self.shared.hello.lock().unwrap() = hello;
    }

    pub fn send_status(&self, line: &str) {
        let _ = self.shared.statuses.send(line.into());
    }

    pub fn send_metrics(&self, line: &str) {
        let _ = self.shared.metrics.send(line.into());
    }

    pub fn doh_queries(&self) -> usize {
        self.shared.doh_queries.load(Ordering::SeqCst)
    }
}

async fn read_head<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        head.push_str(&line);
        if line == "\r\n" {
            return Some(head);
        }
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

fn control(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        while let Some(head) = read_head(&mut r).await {
            let first = head.lines().next().unwrap_or_default().to_string();
            if first.starts_with("GET /cellproxy/hello ") {
                let body = serde_json::to_vec(&*shared.hello.lock().unwrap()).unwrap();
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = w.write_all(reply.as_bytes()).await;
                let _ = w.write_all(&body).await;
                return;
            }
            if let Some(rest) = first.strip_prefix("GET /cellproxy/watch?session=") {
                let wanted = rest.split(' ').next().unwrap_or_default();
                let current = shared.hello.lock().unwrap()["sessionID"].as_str().unwrap_or_default().to_string();
                if wanted != current {
                    let _ = w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
                    return;
                }
                let mut generation = shared.generation.subscribe();
                let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n").await;
                let _ = generation.changed().await;
                return;
            }
            if first.starts_with("POST /dns-query ") {
                let mut query = vec![0u8; content_length(&head)];
                if r.read_exact(&mut query).await.is_err() {
                    return;
                }
                shared.doh_queries.fetch_add(1, Ordering::SeqCst);
                if query.len() > 2 {
                    query[2] |= 0x80; // answer = the query with QR set
                }
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\n\r\n", query.len());
                if w.write_all(reply.as_bytes()).await.is_err() || w.write_all(&query).await.is_err() {
                    return;
                }
                continue;
            }
            let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
            return;
        }
    })
}

fn socks(mut stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut greeting = [0u8; 2];
        if stream.read_exact(&mut greeting).await.is_err() {
            return;
        }
        let mut methods = vec![0u8; greeting[1] as usize];
        if stream.read_exact(&mut methods).await.is_err() || stream.write_all(&[5, 0]).await.is_err() {
            return;
        }
        let mut header = [0u8; 4];
        if stream.read_exact(&mut header).await.is_err() {
            return;
        }
        let mut first = [0u8; 1];
        if stream.read_exact(&mut first).await.is_err() {
            return;
        }
        let Ok(rest) = cg_protocol::socks::address_rest_len(header[3], first[0]) else { return };
        let mut address = vec![0u8; rest];
        if stream.read_exact(&mut address).await.is_err() {
            return;
        }
        match header[1] {
            1 if shared.exit_ok => {
                if stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.is_ok() {
                    let (mut r, mut w) = stream.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                }
            }
            0xf0 if shared.udp_ok => {
                if stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.is_err() {
                    return;
                }
                // Echo every frame back unchanged.
                loop {
                    let mut len = [0u8; 2];
                    if stream.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut payload = vec![0u8; u16::from_be_bytes(len) as usize];
                    if stream.read_exact(&mut payload).await.is_err()
                        || stream.write_all(&len).await.is_err()
                        || stream.write_all(&payload).await.is_err()
                    {
                        return;
                    }
                }
            }
            _ => {
                let _ = stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            }
        }
    })
}

fn lines_from(mut stream: TcpStream, mut rx: broadcast::Receiver<String>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(line) = rx.recv().await {
            if stream.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                return;
            }
        }
    })
}

fn status_lines(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    lines_from(stream, shared.statuses.subscribe())
}

fn metric_lines(stream: TcpStream, shared: Arc<Shared>) -> tokio::task::JoinHandle<()> {
    lines_from(stream, shared.metrics.subscribe())
}
```

`testing/mod.rs` 加：

```rust
mod fake_phone;
pub use fake_phone::{FakePhone, FakePhoneOptions, SESSION_A, SESSION_B, hello_json};
```

`cg-core/Cargo.toml`：`serde_json` 已在依赖里，假手机用到的 `json!` 宏来自它。

- [ ] **Step 2: 写失败的测试**

`crates/cg-core/src/phone.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, SESSION_A, SESSION_B, device, hello_json};
    use std::time::Duration;

    async fn setup(options: FakePhoneOptions) -> (FakeMux, FakePhone, Phone) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, options).await;
        let phone = Phone { endpoint: mux.endpoint(), device: device("aaaa", 1) };
        (mux, fake, phone)
    }

    #[tokio::test]
    async fn hello_reads_capabilities() {
        let (_mux, _fake, phone) = setup(FakePhoneOptions::default()).await;
        let hello = read_hello(&phone).await.unwrap();
        hello.validate().unwrap();
        assert_eq!(hello.session_id, SESSION_A);
        assert!(hello.has(cg_protocol::phone::capability::UDP_OVER_USB));
    }

    #[tokio::test]
    async fn watch_ends_when_phone_ends_session() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let ended = watch(&phone, SESSION_A, Some("测试电脑")).await.unwrap();
        let mut ended = Box::pin(ended);
        assert!(tokio::time::timeout(Duration::from_millis(200), &mut ended).await.is_err(), "still running");
        fake.end_session();
        tokio::time::timeout(Duration::from_secs(2), ended).await.expect("watch should end");
    }

    #[tokio::test]
    async fn watch_with_stale_session_is_rejected() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        fake.set_hello(hello_json(&["socks5-tcp"], true, SESSION_B));
        assert!(matches!(watch(&phone, SESSION_A, None).await, Err(CoreError::SessionChanged)));
    }

    #[tokio::test]
    async fn status_stream_rejects_out_of_order_frames() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(status_stream(phone, tx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let line = |rev: u64| format!(r#"{{"service":"CellGlide-status","version":1,"instance":"{SESSION_A}","revision":{rev},"running":true,"starting":false,"background":true,"dnsEnabled":false}}"#);
        fake.send_status(&line(1));
        assert!(!rx.recv().await.unwrap().dns_enabled);
        fake.send_status(&line(1));
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn metrics_stream_delivers_frames() {
        let (_mux, fake, phone) = setup(FakePhoneOptions::default()).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(metrics_stream(phone, tx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        fake.send_metrics(r#"{"service":"CellGlide-metrics","version":1,"battery":0.5,"download":2048}"#);
        let m = rx.recv().await.unwrap();
        assert_eq!(m.battery, Some(0.5));
        assert_eq!(m.download, Some(2048.0));
    }
}
```

`lib.rs` 加 `pub mod phone;`。

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-core phone`
Expected: 编译失败。

- [ ] **Step 4: 实现 `phone.rs`**

```rust
//! The phone app's control services: hello, session watch, status and metrics.

use crate::mux::{self, MuxEndpoint, MuxStream};
use crate::{CoreError, Result};
use cg_protocol::ProtocolError;
use cg_protocol::mux::Device;
use cg_protocol::phone::{
    HELLO_REQUEST, Hello, MAX_HELLO_BODY, MAX_HTTP_HEAD, MAX_METRICS_LINE, MAX_STATUS_LINE, PhoneMetrics, SharingStatus,
    StatusSequence, parse_http_head, port, watch_request,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A phone that sends no metrics for this long (app in background, cable pulled) ends the stream.
pub const METRICS_SILENCE: Duration = Duration::from_secs(5);

pub type Ended = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Debug)]
pub struct Phone {
    pub endpoint: MuxEndpoint,
    pub device: Device,
}

impl Phone {
    pub async fn open(&self, port: u16) -> Result<MuxStream> {
        mux::connect(&self.endpoint, self.device.mux_id, port).await
    }
}

async fn read_head<S: AsyncRead + Unpin + ?Sized>(stream: &mut S) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HTTP_HEAD {
            return Err(ProtocolError::TooLarge.into());
        }
        head.push(stream.read_u8().await?);
    }
    Ok(head)
}

pub async fn read_hello(phone: &Phone) -> Result<Hello> {
    timeout(HANDSHAKE_TIMEOUT, async {
        let mut stream = phone.open(port::CONTROL).await?;
        stream.write_all(HELLO_REQUEST).await?;
        let head = parse_http_head(&read_head(stream.as_mut()).await?)?;
        let len = match (head.status, head.content_length) {
            (200, Some(n)) if (1..=MAX_HELLO_BODY).contains(&n) => n,
            _ => return Err(CoreError::PhoneTooOld),
        };
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body).map_err(ProtocolError::from)?)
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}

/// Subscribes to the phone's session. The returned future completes when the phone
/// ends the session (stops sharing, quits, or the cable goes).
pub async fn watch(phone: &Phone, session_id: &str, computer_name: Option<&str>) -> Result<Ended> {
    let mut stream = phone.open(port::CONTROL).await?;
    stream.write_all(watch_request(session_id, computer_name, None).as_bytes()).await?;
    let head = timeout(HANDSHAKE_TIMEOUT, read_head(stream.as_mut())).await.map_err(|_| CoreError::Timeout)??;
    if !head.starts_with(b"HTTP/1.1 200 OK\r\n") {
        return Err(CoreError::SessionChanged);
    }
    Ok(Box::pin(async move {
        let mut byte = [0u8; 1];
        let _ = stream.read(&mut byte).await;
    }))
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = reader.read_u8().await?;
        if byte == b'\n' {
            return Ok(line);
        }
        line.push(byte);
        if line.len() >= max {
            return Err(ProtocolError::TooLarge.into());
        }
    }
}

pub async fn status_stream(phone: Phone, tx: mpsc::Sender<SharingStatus>) -> Result<()> {
    let mut reader = BufReader::new(phone.open(port::STATUS).await?);
    let mut sequence = StatusSequence::default();
    loop {
        let line = read_line(&mut reader, MAX_STATUS_LINE).await?;
        let status: SharingStatus = serde_json::from_slice(&line).map_err(ProtocolError::from)?;
        sequence.accept(&status)?;
        if tx.send(status).await.is_err() {
            return Ok(());
        }
    }
}

pub async fn metrics_stream(phone: Phone, tx: mpsc::Sender<PhoneMetrics>) -> Result<()> {
    let mut reader = BufReader::new(phone.open(port::METRICS).await?);
    loop {
        let line = timeout(METRICS_SILENCE, read_line(&mut reader, MAX_METRICS_LINE))
            .await
            .map_err(|_| CoreError::Timeout)??;
        let metrics: PhoneMetrics = serde_json::from_slice(&line).map_err(ProtocolError::from)?;
        if !metrics.is_valid() {
            return Err(ProtocolError::Malformed("metrics frame").into());
        }
        if tx.send(metrics).await.is_err() {
            return Ok(());
        }
    }
}
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p cg-core phone`
Expected: 5 个测试 `ok`。

- [ ] **Step 6: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-core && git commit -m "cg-core: phone hello, session watch, status and metrics streams

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: 本地 SOCKS 转发、UDP 桥与出口检测

参照 `CellProxy:Sources/DesktopCore/USBTransport.swift`（`USBForwarder.handle`、`negotiateUDP`、`associateUDP`）、`USBUDPBridge.swift`、`CellProxy:Docs/USB-UDP-v1.md`。不含分流（第 0 阶段不做）。

**Files:**
- Create: `crates/cg-core/src/forwarder.rs`, `crates/cg-core/src/udp_bridge.rs`
- Modify: `crates/cg-core/src/lib.rs`（加 `pub mod forwarder; mod udp_bridge;`）

**Interfaces:**
- Consumes: `Phone::open`（Task 8）；`cg_protocol::socks::*`（Task 5）
- Produces:
  - `pub struct Forwarder`：`async fn start(phone: Phone) -> Result<Forwarder>`、`fn port(&self) -> u16`、`fn stop(&self)`；`Drop` 时自动停止
  - `pub const MAX_CLIENTS: usize = 256;`
  - `pub async fn probe_exit(socks_port: u16, host: &str, port: u16) -> bool`（8 秒内经手机 CONNECT 成功即为真）
  - `pub(crate) async fn read_address<S: AsyncRead + Unpin + ?Sized>(s: &mut S, atyp: u8) -> Result<Vec<u8>>`（返回 ATYP 之后的全部地址字节，含端口）

- [ ] **Step 1: 写失败的测试**

`crates/cg-core/src/forwarder.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, device};
    use std::time::Duration;
    use tokio::net::{TcpStream, UdpSocket};

    async fn setup(options: FakePhoneOptions) -> (FakeMux, FakePhone, Forwarder) {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, options).await;
        let fwd = Forwarder::start(Phone { endpoint: mux.endpoint(), device: device("aaaa", 1) }).await.unwrap();
        (mux, fake, fwd)
    }

    async fn socks_hello(port: u16) -> TcpStream {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(&[5, 1, 0]).await.unwrap();
        let mut m = [0u8; 2];
        s.read_exact(&mut m).await.unwrap();
        assert_eq!(m, [5, 0]);
        s
    }

    #[tokio::test]
    async fn tcp_connect_relays_both_ways() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let mut s = socks_hello(fwd.port()).await;
        s.write_all(&[5, 1, 0, 1, 1, 2, 3, 4, 0, 80]).await.unwrap();
        let mut reply = [0u8; 10];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        s.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn udp_associate_bridges_frames_and_ignores_other_senders() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let mut control = socks_hello(fwd.port()).await;
        control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..8], &[5, 0, 0, 1, 127, 0, 0, 1]);
        let relay = u16::from_be_bytes([reply[8], reply[9]]);

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let packet = [0, 0, 0, 1, 1, 1, 1, 1, 0, 53, b'q', b'1'];
        client.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], &packet);

        let intruder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        intruder.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(300), intruder.recv(&mut buf)).await.is_err());

        // Zero-length payloads survive.
        let empty = [0, 0, 0, 1, 1, 1, 1, 1, 0, 53];
        client.send_to(&empty, ("127.0.0.1", relay)).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], &empty);

        drop(control);
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.send_to(&packet, ("127.0.0.1", relay)).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(300), client.recv(&mut buf)).await.is_err(), "association closed with its control connection");
    }

    #[tokio::test]
    async fn phone_refusing_udp_gets_command_failed() {
        let (_m, _p, fwd) = setup(FakePhoneOptions { udp_ok: false, ..FakePhoneOptions::default() }).await;
        let mut control = socks_hello(fwd.port()).await;
        control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        let mut reply = [0u8; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, cg_protocol::socks::REPLY_COMMAND_FAILED);
    }

    #[tokio::test]
    async fn exit_probe_follows_phone() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        assert!(probe_exit(fwd.port(), "www.cloudflare.com", 443).await);
        let (_m2, _p2, dead) = setup(FakePhoneOptions { exit_ok: false, ..FakePhoneOptions::default() }).await;
        assert!(!probe_exit(dead.port(), "www.cloudflare.com", 443).await);
    }

    #[tokio::test]
    async fn stop_closes_listener() {
        let (_m, _p, fwd) = setup(FakePhoneOptions::default()).await;
        let port = fwd.port();
        drop(fwd);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }
}
```

`lib.rs` 加：

```rust
pub mod forwarder;
mod udp_bridge;
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p cg-core forwarder`
Expected: 编译失败。

- [ ] **Step 3: 实现 `forwarder.rs`**

```rust
//! The local SOCKS5 endpoint hev talks to. Every client gets its own channel to the
//! phone's SOCKS port; UDP ASSOCIATE is turned into the phone's udp-over-usb-v1.

use crate::phone::Phone;
use crate::udp_bridge;
use crate::{CoreError, Result};
use cg_protocol::phone::port;
use cg_protocol::socks::{address_rest_len, connect_domain_request};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Matches the phone's session limit.
pub const MAX_CLIENTS: usize = 256;
pub const EXIT_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

pub struct Forwarder {
    port: u16,
    cancel: CancellationToken,
}

impl Forwarder {
    pub async fn start(phone: Phone) -> Result<Forwarder> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = token.cancelled() => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((client, _)) = accepted else {
                    // Out of descriptors: back off instead of giving up on the port.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let phone = phone.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    let _ = client.set_nodelay(true);
                    tokio::select! {
                        _ = token.cancelled() => {}
                        _ = handle(client, phone) => {}
                    }
                });
            }
        });
        Ok(Forwarder { port, cancel })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The address bytes after ATYP, port included.
pub(crate) async fn read_address<S: AsyncRead + Unpin + ?Sized>(stream: &mut S, atyp: u8) -> Result<Vec<u8>> {
    let first = stream.read_u8().await?;
    let mut address = vec![first];
    address.resize(1 + address_rest_len(atyp, first)?, 0);
    stream.read_exact(&mut address[1..]).await?;
    Ok(address)
}

async fn handle(mut client: TcpStream, phone: Phone) -> Result<()> {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    if greeting[0] != 5 || greeting[1] == 0 {
        return Err(cg_protocol::ProtocolError::Malformed("socks greeting").into());
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods).await?;

    let mut peer = phone.open(port::SOCKS).await?;
    peer.write_all(&greeting).await?;
    peer.write_all(&methods).await?;
    let mut method = [0u8; 2];
    peer.read_exact(&mut method).await?;
    client.write_all(&method).await?;
    if method != [5, 0] {
        return Ok(());
    }

    let mut header = [0u8; 4];
    client.read_exact(&mut header).await?;
    if header[0] != 5 || header[2] != 0 {
        return Err(cg_protocol::ProtocolError::Malformed("socks request").into());
    }
    let address = read_address(&mut client, header[3]).await?;
    if header[1] == 3 {
        return udp_bridge::associate(client, peer, &address).await;
    }
    peer.write_all(&header).await?;
    peer.write_all(&address).await?;
    tokio::io::copy_bidirectional(&mut client, &mut peer).await?;
    Ok(())
}

/// Whether the phone can reach `host:port`: a SOCKS CONNECT through the forwarder.
pub async fn probe_exit(socks_port: u16, host: &str, port: u16) -> bool {
    let attempt = async {
        let mut s = TcpStream::connect(("127.0.0.1", socks_port)).await?;
        s.write_all(&[5, 1, 0]).await?;
        let mut method = [0u8; 2];
        s.read_exact(&mut method).await?;
        if method != [5, 0] {
            return Ok::<bool, CoreError>(false);
        }
        s.write_all(&connect_domain_request(host, port)?).await?;
        let mut reply = [0u8; 4];
        s.read_exact(&mut reply).await?;
        read_address(&mut s, reply[3]).await?;
        Ok(reply[1] == 0)
    };
    matches!(tokio::time::timeout(EXIT_PROBE_TIMEOUT, attempt).await, Ok(Ok(true)))
}
```

- [ ] **Step 4: 实现 `udp_bridge.rs`**

```rust
//! One SOCKS5 UDP association, carried to the phone as udp-over-usb-v1 frames.
//! Only the first local sender (and the requested port, when one was given) is served.

use crate::forwarder::read_address;
use crate::mux::MuxStream;
use crate::Result;
use cg_protocol::socks::{REPLY_COMMAND_FAILED, UDP_OVER_USB_REQUEST, encode_frame, udp_associate_reply};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};

pub(crate) async fn associate(mut client: TcpStream, mut peer: MuxStream, request_address: &[u8]) -> Result<()> {
    let n = request_address.len();
    let requested_port = u16::from_be_bytes([request_address[n - 2], request_address[n - 1]]);
    let udp = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let local_port = udp.local_addr()?.port();

    peer.write_all(&UDP_OVER_USB_REQUEST).await?;
    let mut reply = [0u8; 4];
    peer.read_exact(&mut reply).await?;
    read_address(peer.as_mut(), reply[3]).await?;
    if reply[..3] != [5, 0, 0] {
        client.write_all(&REPLY_COMMAND_FAILED).await?;
        return Ok(());
    }
    client.write_all(&udp_associate_reply(local_port)).await?;

    let source: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let (phone_r, phone_w) = tokio::io::split(peer);
    let control = async move {
        let mut byte = [0u8; 1];
        let _ = client.read(&mut byte).await;
    };
    tokio::select! {
        _ = uplink(udp.clone(), source.clone(), requested_port, phone_w) => {}
        _ = downlink(udp, source, phone_r) => {}
        _ = control => {}
    }
    Ok(())
}

type Source = Arc<Mutex<Option<SocketAddr>>>;

/// Local datagrams to the phone, from the association's one sender.
async fn uplink(udp: Arc<UdpSocket>, source: Source, requested_port: u16, mut phone: WriteHalf<MuxStream>) -> Result<()> {
    let mut buf = vec![0u8; 65535];
    loop {
        let (len, from) = udp.recv_from(&mut buf).await?;
        if !from.ip().is_loopback() {
            continue;
        }
        let accepted = {
            let mut known = source.lock().unwrap();
            match *known {
                Some(addr) => addr == from,
                None if requested_port != 0 && from.port() != requested_port => false,
                None => {
                    *known = Some(from);
                    true
                }
            }
        };
        if accepted {
            phone.write_all(&encode_frame(&buf[..len])?).await?;
        }
    }
}

/// Frames from the phone back to the local sender (dropped until one is known).
async fn downlink(udp: Arc<UdpSocket>, source: Source, mut phone: ReadHalf<MuxStream>) -> Result<()> {
    loop {
        // Frame length is a big-endian u16, which is what read_u16 reads.
        let len = phone.read_u16().await? as usize;
        let mut payload = vec![0u8; len];
        phone.read_exact(&mut payload).await?;
        let target = *source.lock().unwrap();
        if let Some(target) = target {
            udp.send_to(&payload, target).await?;
        }
    }
}
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p cg-core forwarder`
Expected: 5 个测试 `ok`。

- [ ] **Step 6: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-core && git commit -m "cg-core: SOCKS forwarder, udp-over-usb bridge and exit probe

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10: 本地 DNS 解析器、DoH 上游与隧道常量

参照 `CellProxy:Sources/DesktopCore/LocalResolver.swift`（并发、合并、SERVFAIL）、`TunnelConfiguration.swift`。

**Files:**
- Create: `crates/cg-core/src/resolver.rs`, `crates/cg-core/src/doh.rs`, `crates/cg-core/src/tunnel.rs`
- Modify: `crates/cg-core/src/lib.rs`（加 `pub mod resolver; pub mod doh; pub mod tunnel;`）

**Interfaces:**
- Consumes: `Phone`（Task 8）
- Produces:
  - `pub trait Upstream: Send + Sync + 'static { fn exchange(&self, query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>>; }`（`BoxFuture` 用 `Pin<Box<dyn Future<Output = T> + Send + 'a>>`，定义在 `resolver.rs`）
  - `pub struct Resolver`：`async fn start(addr: SocketAddr, upstream: Arc<dyn Upstream>) -> Result<Resolver>`（端口占用返回 `CoreError::ResolverPortBusy(port)`）、`fn local_addr(&self) -> SocketAddr`、`fn stop(&self)`，`Drop` 时停止
  - `pub fn servfail(query: &[u8]) -> Option<Vec<u8>>`
  - 常量 `MAX_IN_FLIGHT = 128`、`UPSTREAM_TIMEOUT = 10s`、`MAX_TCP_CLIENTS = 16`
  - `pub struct DohUpstream`：`fn new(phone: Phone) -> Self`，实现 `Upstream`
  - `tunnel.rs`：`INTERFACE_NAME`、`ADDRESS_V4`、`ADDRESS_V6`、`MTU`、`IPV4_ROUTES`、`IPV6_ROUTES`、`RESOLVER: SocketAddr`、`FALLBACK_DNS: [IpAddr; 4]`、`fn hev_config(socks_port: u16) -> String`、`pub enum DnsPlan { Resolver(SocketAddr), Servers(Vec<IpAddr>) }`、`pub struct TunnelPlan { pub socks_port: u16, pub dns: DnsPlan }`、`pub trait TunnelBackend`、`pub trait RunningTunnel`、`pub struct TunnelError(pub String)`

- [ ] **Step 1: 写失败的测试（解析器）**

`crates/cg-core/src/resolver.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UdpSocket};

    /// Answers with the query plus QR, after `delay`; counts calls.
    struct Echo {
        calls: AtomicUsize,
        delay: Duration,
        fail: bool,
    }

    impl Upstream for Echo {
        fn exchange(&self, mut query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                if self.fail {
                    return None;
                }
                query[2] |= 0x80;
                // Upstream answers with its own ID; the resolver must restore the client's.
                query[0] = 0xAB;
                query[1] = 0xCD;
                Some(query)
            })
        }
    }

    fn query(id: u16) -> Vec<u8> {
        let mut q = id.to_be_bytes().to_vec();
        q.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        q.extend_from_slice(&[7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o', b'm', 0, 0, 1, 0, 1]);
        q
    }

    async fn start(echo: Arc<Echo>) -> Resolver {
        Resolver::start("127.0.0.1:0".parse().unwrap(), echo).await.unwrap()
    }

    #[tokio::test]
    async fn udp_answers_with_client_id() {
        let echo = Arc::new(Echo { calls: AtomicUsize::new(0), delay: Duration::ZERO, fail: false });
        let resolver = start(echo).await;
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(&query(0x1234), resolver.local_addr()).await.unwrap();
        let mut buf = [0u8; 512];
        let n = client.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0x12, 0x34]);
        assert_eq!(buf[2] & 0x80, 0x80);
        assert_eq!(n, query(0).len());
    }

    #[tokio::test]
    async fn identical_questions_share_one_upstream_exchange() {
        let echo = Arc::new(Echo { calls: AtomicUsize::new(0), delay: Duration::from_millis(200), fail: false });
        let resolver = start(echo.clone()).await;
        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        a.send_to(&query(1), resolver.local_addr()).await.unwrap();
        b.send_to(&query(2), resolver.local_addr()).await.unwrap();
        let mut buf = [0u8; 512];
        a.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0, 1]);
        b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..2], &[0, 2]);
        assert_eq!(echo.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_upstream_gives_servfail() {
        let echo = Arc::new(Echo { calls: AtomicUsize::new(0), delay: Duration::ZERO, fail: true });
        let resolver = start(echo).await;
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(&query(7), resolver.local_addr()).await.unwrap();
        let mut buf = [0u8; 512];
        let n = client.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &servfail(&query(7)).unwrap()[..]);
        assert_eq!(buf[3] & 0x0f, 2);
    }

    #[tokio::test]
    async fn tcp_queries_are_length_prefixed() {
        let echo = Arc::new(Echo { calls: AtomicUsize::new(0), delay: Duration::ZERO, fail: false });
        let resolver = start(echo).await;
        let mut s = TcpStream::connect(resolver.local_addr()).await.unwrap();
        let q = query(9);
        s.write_all(&(q.len() as u16).to_be_bytes()).await.unwrap();
        s.write_all(&q).await.unwrap();
        let len = s.read_u16().await.unwrap() as usize;
        let mut reply = vec![0u8; len];
        s.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], &[0, 9]);
    }

    #[tokio::test]
    async fn busy_port_is_reported() {
        let taken = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo = Arc::new(Echo { calls: AtomicUsize::new(0), delay: Duration::ZERO, fail: false });
        let err = Resolver::start(taken.local_addr().unwrap(), echo).await.unwrap_err();
        assert!(matches!(err, CoreError::ResolverPortBusy(_)));
        assert!(err.to_string().contains("已被占用"));
    }

    #[test]
    fn servfail_keeps_question_and_flags() {
        let q = query(0x0102);
        let r = servfail(&q).unwrap();
        assert_eq!(&r[..2], &[1, 2]);
        assert_eq!(r[2], 0x80 | 0x01, "QR set, RD kept");
        assert_eq!(r[3], 0x82, "RA, SERVFAIL");
        assert_eq!(&r[4..6], &[0, 1]);
        assert_eq!(&r[6..12], &[0; 6]);
        assert_eq!(&r[12..], &q[12..]);
        let mut response = q.clone();
        response[2] |= 0x80;
        assert!(servfail(&response).is_none(), "never answer a response");
        assert!(servfail(&q[..5]).is_none());
    }
}
```

- [ ] **Step 2: 写失败的测试（DoH 与隧道常量）**

`crates/cg-core/src/doh.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, device};

    #[tokio::test]
    async fn posts_queries_over_one_kept_alive_connection() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let doh = DohUpstream::new(Phone { endpoint: mux.endpoint(), device: device("aaaa", 1) });
        for id in 0..3u8 {
            let query = vec![0, id, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1];
            let answer = doh.exchange(query.clone()).await.unwrap();
            assert_eq!(answer[1], id);
            assert_eq!(answer[2] & 0x80, 0x80);
        }
        assert_eq!(fake.doh_queries(), 3);
    }

    #[tokio::test]
    async fn unreachable_phone_gives_none() {
        let mux = FakeMux::start().await;
        let doh = DohUpstream::new(Phone { endpoint: mux.endpoint(), device: device("gone", 9) });
        assert!(doh.exchange(vec![0; 17]).await.is_none());
    }
}
```

`crates/cg-core/src/tunnel.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hev_config_matches_mac() {
        assert_eq!(
            hev_config(40000),
            "tunnel:\n  mtu: 1500\nsocks5:\n  port: 40000\n  address: 127.0.0.1\n  udp: 'udp'\nmisc:\n  log-level: warn\n  log-file: stderr\n  tcp-read-write-timeout: 1800000\n  udp-read-write-timeout: 30000\n"
        );
    }

    #[test]
    fn constants_match_mac() {
        assert_eq!(INTERFACE_NAME, "cellglide0");
        assert_eq!(ADDRESS_V4.to_string(), "198.18.0.1");
        assert_eq!(ADDRESS_V6.to_string(), "fd6c:6779:6465::1");
        assert_eq!(IPV4_ROUTES, ["0.0.0.0/1", "128.0.0.0/1"]);
        assert_eq!(IPV6_ROUTES, ["::/1", "8000::/1"]);
        assert_eq!(FALLBACK_DNS.map(|ip| ip.to_string()), ["2606:4700:4700::1111", "2001:4860:4860::8888", "1.1.1.1", "8.8.8.8"]);
    }
}
```

`lib.rs` 加：

```rust
pub mod doh;
pub mod resolver;
pub mod tunnel;
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-core resolver doh tunnel`（三个过滤器分三次跑也可：`cargo test -p cg-core resolver`）
Expected: 编译失败。

- [ ] **Step 4: 实现 `tunnel.rs`**

```rust
//! The tunnel's fixed parts, identical to the Mac's TunnelConfiguration, and the
//! interface a platform implements to bring the tunnel up.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const INTERFACE_NAME: &str = "cellglide0";
pub const ADDRESS_V4: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
pub const ADDRESS_V6: Ipv6Addr = Ipv6Addr::new(0xfd6c, 0x6779, 0x6465, 0, 0, 0, 0, 1);
pub const MTU: u32 = 1500;
pub const IPV4_ROUTES: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];
pub const IPV6_ROUTES: [&str; 2] = ["::/1", "8000::/1"];
/// The local resolver systemd-resolved sends this link's queries to. Loopback is never
/// routed into the tunnel. (If Task 2's probe showed resolved refusing a port here,
/// this is 198.18.0.1:53 instead.)
pub const RESOLVER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1053);
/// With the phone's CellGlide DNS off: plain DNS to public resolvers through the tunnel.
/// IPv6 first, since some cellular networks only reach IPv4 literals through translation.
pub const FALLBACK_DNS: [IpAddr; 4] = [
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
    IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
];

/// hev-socks5-tunnel settings. Idle UDP sessions each hold a SOCKS connection on the
/// phone, so they are released after 30 s rather than hev's default 60 s.
pub fn hev_config(socks_port: u16) -> String {
    format!(
        "tunnel:\n  mtu: {MTU}\nsocks5:\n  port: {socks_port}\n  address: 127.0.0.1\n  udp: 'udp'\nmisc:\n  log-level: warn\n  log-file: stderr\n  tcp-read-write-timeout: 1800000\n  udp-read-write-timeout: 30000\n"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DnsPlan {
    /// Our resolver, which forwards to the phone's DNS-over-HTTPS.
    Resolver(SocketAddr),
    /// Public servers on port 53, reached through the tunnel.
    Servers(Vec<IpAddr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelPlan {
    pub socks_port: u16,
    pub dns: DnsPlan,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TunnelError(pub String);

/// Brings up the interface, routes, DNS and hev in one go, or leaves nothing behind.
/// Called from a blocking thread.
pub trait TunnelBackend: Send + Sync + 'static {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError>;
}

pub trait RunningTunnel: Send + 'static {
    /// Undo everything `start` did. Called from a blocking thread.
    fn stop(self: Box<Self>) -> Result<(), TunnelError>;
    /// hev returned on its own (a crash or a fatal error).
    fn has_exited(&self) -> bool;
}
```

- [ ] **Step 5: 实现 `resolver.rs`**

```rust
//! Local DNS server for the tunnel link. Takes queries over UDP and TCP and hands
//! each to the upstream (DNS-over-HTTPS to the phone). Identical questions in flight
//! share one exchange; answers pass through unchanged apart from the client's ID.

use crate::{CoreError, Result};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_IN_FLIGHT: usize = 128;
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_TCP_CLIENTS: usize = 16;
const TCP_IDLE: Duration = Duration::from_secs(10);

pub trait Upstream: Send + Sync + 'static {
    fn exchange(&self, query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>>;
}

type Waiters = Arc<Mutex<HashMap<Vec<u8>, Vec<oneshot::Sender<Option<Vec<u8>>>>>>>;

pub struct Resolver {
    addr: SocketAddr,
    cancel: CancellationToken,
}

impl Resolver {
    pub async fn start(addr: SocketAddr, upstream: Arc<dyn Upstream>) -> Result<Resolver> {
        let busy = |_| CoreError::ResolverPortBusy(addr.port());
        let udp = Arc::new(UdpSocket::bind(addr).await.map_err(busy)?);
        let addr = udp.local_addr()?;
        let tcp = TcpListener::bind(addr).await.map_err(|_| CoreError::ResolverPortBusy(addr.port()))?;
        let cancel = CancellationToken::new();
        let waiters: Waiters = Arc::default();

        let token = cancel.clone();
        let (up, w, sock) = (upstream.clone(), waiters.clone(), udp.clone());
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = serve_udp(sock, up, w) => {}
            }
        });
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = serve_tcp(tcp, upstream, waiters, token.clone()) => {}
            }
        });
        Ok(Resolver { addr, cancel })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A SERVFAIL reply to `query`, keeping its question; None for a response or a runt.
pub fn servfail(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None;
    }
    let mut end = 12;
    if query[4] == 0 && query[5] == 1 {
        while end < query.len() && query[end] != 0 && query[end] & 0xc0 == 0 {
            end += query[end] as usize + 1;
        }
        end += 5; // root label, QTYPE, QCLASS
    }
    let has_question = end > 12 && end <= query.len();
    let mut reply = query[..if has_question { end } else { 12 }].to_vec();
    reply[2] = 0x80 | (query[2] & 0x79); // QR, keep opcode and RD
    reply[3] = 0x80 | 2; // RA, SERVFAIL
    reply[4] = 0;
    reply[5] = u8::from(has_question);
    reply[6..12].fill(0);
    Some(reply)
}

async fn resolve(query: Vec<u8>, upstream: &Arc<dyn Upstream>, waiters: &Waiters) -> Option<Vec<u8>> {
    let mut key = query.clone();
    key[0] = 0;
    key[1] = 0;
    let (tx, rx) = oneshot::channel();
    let first = {
        let mut map = waiters.lock().unwrap();
        if let Some(list) = map.get_mut(&key) {
            list.push(tx);
            false
        } else if map.len() >= MAX_IN_FLIGHT {
            return servfail(&query);
        } else {
            map.insert(key.clone(), vec![tx]);
            true
        }
    };
    if first {
        let upstream = upstream.clone();
        let waiters = waiters.clone();
        let q = query.clone();
        tokio::spawn(async move {
            let reply = tokio::time::timeout(UPSTREAM_TIMEOUT, upstream.exchange(q)).await.ok().flatten();
            let clients = waiters.lock().unwrap().remove(&key).unwrap_or_default();
            for client in clients {
                let _ = client.send(reply.clone());
            }
        });
    }
    match rx.await.ok().flatten() {
        Some(mut reply) if reply.len() >= 12 => {
            reply[0] = query[0];
            reply[1] = query[1];
            Some(reply)
        }
        _ => servfail(&query),
    }
}

async fn serve_udp(socket: Arc<UdpSocket>, upstream: Arc<dyn Upstream>, waiters: Waiters) {
    let mut buf = vec![0u8; 65535];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf).await else { continue };
        if len < 12 {
            continue;
        }
        let query = buf[..len].to_vec();
        let (socket, upstream, waiters) = (socket.clone(), upstream.clone(), waiters.clone());
        tokio::spawn(async move {
            if let Some(reply) = resolve(query, &upstream, &waiters).await {
                let _ = socket.send_to(&reply, from).await;
            }
        });
    }
}

async fn serve_tcp(listener: TcpListener, upstream: Arc<dyn Upstream>, waiters: Waiters, cancel: CancellationToken) {
    let slots = Arc::new(Semaphore::new(MAX_TCP_CLIENTS));
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
        let (upstream, waiters, cancel) = (upstream.clone(), waiters.clone(), cancel.clone());
        tokio::spawn(async move {
            let _slot = slot;
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = serve_tcp_client(stream, upstream, waiters) => {}
            }
        });
    }
}

async fn serve_tcp_client(mut stream: TcpStream, upstream: Arc<dyn Upstream>, waiters: Waiters) -> Option<()> {
    loop {
        let len = tokio::time::timeout(TCP_IDLE, stream.read_u16()).await.ok()?.ok()? as usize;
        if len < 12 {
            return None;
        }
        let mut query = vec![0u8; len];
        tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut query)).await.ok()?.ok()?;
        let reply = resolve(query, &upstream, &waiters).await?;
        let len = u16::try_from(reply.len()).ok()?;
        stream.write_all(&len.to_be_bytes()).await.ok()?;
        stream.write_all(&reply).await.ok()?;
    }
}
```

- [ ] **Step 6: 实现 `doh.rs`**

```rust
//! DNS-over-HTTPS (RFC 8484 POST) to the phone's control port. The phone speaks plain
//! HTTP/1.1 on loopback; usbmuxd carries it. Idle connections are kept for reuse.

use crate::phone::Phone;
use crate::resolver::{BoxFuture, Upstream};
use bytes::Bytes;
use cg_protocol::phone::port;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use std::sync::Mutex;

const MAX_IDLE: usize = 4;
const DNS_MESSAGE: &str = "application/dns-message";

pub struct DohUpstream {
    phone: Phone,
    idle: Mutex<Vec<SendRequest<Full<Bytes>>>>,
}

impl DohUpstream {
    pub fn new(phone: Phone) -> Self {
        Self { phone, idle: Mutex::new(Vec::new()) }
    }

    fn take_idle(&self) -> Option<SendRequest<Full<Bytes>>> {
        let mut idle = self.idle.lock().unwrap();
        while let Some(sender) = idle.pop() {
            if !sender.is_closed() {
                return Some(sender);
            }
        }
        None
    }

    async fn connect(&self) -> Option<SendRequest<Full<Bytes>>> {
        let stream = self.phone.open(port::CONTROL).await.ok()?;
        let (sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
        tokio::spawn(connection);
        Some(sender)
    }

    async fn post(&self, mut sender: SendRequest<Full<Bytes>>, query: Bytes) -> Option<Vec<u8>> {
        sender.ready().await.ok()?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/dns-query")
            .header(HOST, "localhost")
            .header(CONTENT_TYPE, DNS_MESSAGE)
            .header(ACCEPT, DNS_MESSAGE)
            .body(Full::new(query))
            .ok()?;
        let response = sender.send_request(request).await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.into_body().collect().await.ok()?.to_bytes();
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_IDLE {
            idle.push(sender);
        }
        Some(body.to_vec())
    }
}

impl Upstream for DohUpstream {
    fn exchange(&self, query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let query = Bytes::from(query);
            // The phone closes idle keep-alive connections; a POST that lands on one
            // fails, so it is retried once on a fresh connection.
            if let Some(sender) = self.take_idle() {
                if let Some(answer) = self.post(sender, query.clone()).await {
                    return Some(answer);
                }
            }
            let sender = self.connect().await?;
            self.post(sender, query).await
        })
    }
}
```

- [ ] **Step 7: 运行，确认通过**

Run: `cargo test -p cg-core resolver && cargo test -p cg-core doh && cargo test -p cg-core tunnel`
Expected: 解析器 6 个、DoH 2 个、隧道 2 个测试全部 `ok`。

- [ ] **Step 8: 若 Task 2 结论为「不接受」**

把 `RESOLVER` 改为 `SocketAddr::new(IpAddr::V4(ADDRESS_V4), 53)`，并删掉注释里的条件说明。否则跳过。

- [ ] **Step 9: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-core && git commit -m "cg-core: local DNS resolver, DoH upstream and tunnel constants

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: 连接状态机（`cg-core::controller`）

参照 `CellProxy:Desktop/macOS/CellProxyMac/App/DesktopModel.swift`（约 930–1110 行的检查流程）与 `ConnectionPolicy.swift`。第 0 阶段做法：用户 `connect` 后持续尝试（失败后按 2、4、8、16、30 秒退避），会话结束或 hev 退出立即拆隧道并马上重试；`disconnect` 后停止。

**Files:**
- Create: `crates/cg-core/src/controller.rs`, `crates/cg-core/src/testing/fake_backend.rs`
- Modify: `crates/cg-core/src/lib.rs`（加 `pub mod controller;`）、`crates/cg-core/src/testing/mod.rs`（加 `mod fake_backend; pub use fake_backend::{FakeBackend, SharedFake};`）

**Interfaces:**
- Consumes: `mux::{listen, list_devices}`、`Phone`、`phone::{read_hello, watch, status_stream, metrics_stream}`、`Forwarder`、`probe_exit`、`Resolver`、`DohUpstream`、`tunnel::*`、`cg_ipc::{ConnState, Snapshot, MetricsSummary}`
- Produces:
  - `pub struct ControllerConfig { pub endpoint: MuxEndpoint, pub backend: Arc<dyn TunnelBackend>, pub resolver_addr: SocketAddr, pub computer_name: Option<String>, pub exit_target: (String, u16), pub relisten_delay: Duration }`；`ControllerConfig::new(endpoint, backend) -> Self`（填默认值：`RESOLVER`、主机名 `None`、`("www.cloudflare.com", 443)`、2 秒）
  - `pub enum Command { Connect { serial: Option<String> }, Disconnect }`
  - `#[derive(Clone)] pub struct Controller`：`fn spawn(ControllerConfig) -> Controller`、`async fn send(&self, Command) -> Result<()>`、`fn snapshot(&self) -> Snapshot`、`fn subscribe(&self) -> watch::Receiver<Snapshot>`、`async fn shutdown(&self)`（拆隧道后退出）
  - 测试工具 `FakeBackend`：`new() -> Arc<FakeBackend>`、`started() -> usize`、`stopped() -> usize`、`last_plan() -> Option<TunnelPlan>`、`exit_running()`（模拟 hev 退出）；`SharedFake(pub Arc<FakeBackend>)` 实现 `TunnelBackend`，交给控制器用

- [ ] **Step 1: 假后端**

`crates/cg-core/src/testing/fake_backend.rs`：

```rust
use crate::tunnel::{RunningTunnel, TunnelBackend, TunnelError, TunnelPlan};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Records what the controller asks of the platform.
#[derive(Default)]
pub struct FakeBackend {
    started: AtomicUsize,
    stopped: AtomicUsize,
    plans: Mutex<Vec<TunnelPlan>>,
    exited: Arc<AtomicBool>,
}

struct FakeRunning {
    backend: Arc<FakeBackend>,
    exited: Arc<AtomicBool>,
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
    pub fn stopped(&self) -> usize {
        self.stopped.load(Ordering::SeqCst)
    }
    pub fn last_plan(&self) -> Option<TunnelPlan> {
        self.plans.lock().unwrap().last().cloned()
    }
    /// The running tunnel reports that hev returned.
    pub fn exit_running(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }
}

/// `Arc<FakeBackend>` is what tests hold; the controller gets a clone of the same Arc.
pub struct SharedFake(pub Arc<FakeBackend>);

impl TunnelBackend for SharedFake {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError> {
        let backend = &self.0;
        backend.exited.store(false, Ordering::SeqCst);
        backend.started.fetch_add(1, Ordering::SeqCst);
        backend.plans.lock().unwrap().push(plan.clone());
        Ok(Box::new(FakeRunning { backend: backend.clone(), exited: backend.exited.clone() }))
    }
}

impl RunningTunnel for FakeRunning {
    fn stop(self: Box<Self>) -> Result<(), TunnelError> {
        self.backend.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}
```

`testing/mod.rs` 加：

```rust
mod fake_backend;
pub use fake_backend::{FakeBackend, SharedFake};
```

- [ ] **Step 2: 写失败的测试**

`crates/cg-core/src/controller.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeBackend, FakeMux, FakePhone, FakePhoneOptions, SESSION_A, SharedFake, device, hello_json};
    use cg_ipc::ConnState;

    struct Rig {
        mux: FakeMux,
        phone: FakePhone,
        backend: Arc<FakeBackend>,
        controller: Controller,
    }

    async fn rig(options: FakePhoneOptions, resolver_addr: SocketAddr) -> Rig {
        let mux = FakeMux::start().await;
        let phone = FakePhone::start(&mux, options).await;
        let backend = FakeBackend::new();
        let mut config = ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(backend.clone())));
        config.resolver_addr = resolver_addr;
        config.relisten_delay = Duration::from_millis(200);
        let controller = Controller::spawn(config);
        Rig { mux, phone, backend, controller }
    }

    fn any_port() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    async fn wait_for(c: &Controller, what: &str, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let mut rx = c.subscribe();
        let found = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let snap = rx.borrow_and_update().clone();
                if pred(&snap) {
                    return snap;
                }
                rx.changed().await.unwrap();
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("timed out waiting for {what}; last: {:?}", c.snapshot()))
    }

    fn connected(s: &Snapshot) -> bool {
        matches!(s.state, ConnState::Connected { .. })
    }

    #[tokio::test]
    async fn connects_one_phone() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle).await;
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.state, ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: true });
        assert_eq!(snap.devices, ["aaaa"]);
        assert_eq!(r.backend.started(), 1);
        let plan = r.backend.last_plan().unwrap();
        assert!(matches!(plan.dns, DnsPlan::Resolver(addr) if addr.port() != 0));
        assert_ne!(plan.socks_port, 0);
    }

    #[tokio::test]
    async fn session_end_tears_down_and_reconnects() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.phone.end_session();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| connected(s) && r.backend.started() == 2).await;
    }

    #[tokio::test]
    async fn detach_tears_down() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.mux.detach(1);
        wait_for(&r.controller, "no phone", |s| s.state == ConnState::NoPhone).await;
        assert_eq!(r.backend.stopped(), 1);
    }

    #[tokio::test]
    async fn disconnect_stops_and_stays_idle() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.controller.send(Command::Disconnect).await.unwrap();
        wait_for(&r.controller, "idle", |s| s.state == ConnState::Idle && !s.wants_connection).await;
        assert_eq!(r.backend.stopped(), 1);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 1);
    }

    #[tokio::test]
    async fn old_phone_is_told_to_update() {
        let options = FakePhoneOptions { hello: hello_json(&["socks5-tcp", "session-watch-v1"], true, SESSION_A), ..FakePhoneOptions::default() };
        let r = rig(options, any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| matches!(s.state, ConnState::Retrying { .. })).await;
        assert!(matches!(snap.state, ConnState::Retrying { ref reason } if reason.contains("太旧")));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn phone_dns_off_uses_public_servers() {
        let caps = ["socks5-tcp", "udp-over-usb-v1", "dns-forward-v1", "session-watch-v1"];
        let options = FakePhoneOptions { hello: hello_json(&caps, false, SESSION_A), ..FakePhoneOptions::default() };
        let r = rig(options, any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.state, ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: false });
        assert_eq!(r.backend.last_plan().unwrap().dns, DnsPlan::Servers(FALLBACK_DNS.to_vec()));
    }

    #[tokio::test]
    async fn no_exit_does_not_start_tunnel() {
        let r = rig(FakePhoneOptions { exit_ok: false, ..FakePhoneOptions::default() }, any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| matches!(s.state, ConnState::Retrying { .. })).await;
        assert!(matches!(snap.state, ConnState::Retrying { ref reason } if reason.contains("手机网络暂时不通")));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn two_phones_require_choice() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.mux.attach(device("bbbb", 2));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "choose", |s| s.state == ConnState::ChoosePhone).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(r.backend.started(), 0);
        r.controller.send(Command::Connect { serial: Some("bbbb".into()) }).await.unwrap();
        let snap = wait_for(&r.controller, "connected", connected).await;
        assert_eq!(snap.selected.as_deref(), Some("bbbb"));
    }

    #[tokio::test]
    async fn resolver_port_busy_fails_cleanly() {
        let taken = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let r = rig(FakePhoneOptions::default(), taken.local_addr().unwrap()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        let snap = wait_for(&r.controller, "retrying", |s| matches!(s.state, ConnState::Retrying { .. })).await;
        assert!(matches!(snap.state, ConnState::Retrying { ref reason } if reason.contains("已被占用")));
        assert_eq!(r.backend.started(), 0);
    }

    #[tokio::test]
    async fn usbmuxd_restart_tears_down_and_relistens() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.mux.drop_listeners();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| connected(s) && r.backend.started() == 2).await;
    }

    #[tokio::test]
    async fn hev_exit_tears_down_and_reconnects() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        r.backend.exit_running();
        wait_for(&r.controller, "torn down", |_| r.backend.stopped() == 1).await;
        wait_for(&r.controller, "reconnected", |s| connected(s) && r.backend.started() == 2).await;
    }

    #[tokio::test]
    async fn missing_usbmuxd_is_reported() {
        let backend = FakeBackend::new();
        let controller = Controller::spawn(ControllerConfig::new(
            MuxEndpoint::Unix("/nonexistent/usbmuxd".into()),
            Arc::new(SharedFake(backend)),
        ));
        wait_for(&controller, "no usb service", |s| s.state == ConnState::NoUsbService).await;
    }

    #[tokio::test]
    async fn metrics_reach_the_snapshot() {
        let r = rig(FakePhoneOptions::default(), any_port()).await;
        r.mux.attach(device("aaaa", 1));
        r.controller.send(Command::Connect { serial: None }).await.unwrap();
        wait_for(&r.controller, "connected", connected).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        r.phone.send_metrics(r#"{"service":"CellGlide-metrics","version":1,"carrier":"中国移动","battery":0.6}"#);
        let snap = wait_for(&r.controller, "metrics", |s| s.metrics.is_some()).await;
        let m = snap.metrics.unwrap();
        assert_eq!(m.carrier.as_deref(), Some("中国移动"));
        assert_eq!(m.battery, Some(0.6));
    }
}
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-core controller`
Expected: 编译失败。

- [ ] **Step 4: 实现 `controller.rs`**

```rust
//! The connection state machine. One task owns all state; usbmuxd events, user
//! commands and session events arrive as messages. Every change is published as a
//! Snapshot on a watch channel.

use crate::doh::DohUpstream;
use crate::forwarder::{Forwarder, probe_exit};
use crate::mux::{self, MuxEndpoint};
use crate::phone::{self, Ended, Phone};
use crate::resolver::Resolver;
use crate::tunnel::{DnsPlan, FALLBACK_DNS, RESOLVER, RunningTunnel, TunnelBackend, TunnelPlan};
use crate::{CoreError, Result};
use cg_ipc::{ConnState, MetricsSummary, Snapshot};
use cg_protocol::mux::{Device, MuxEvent};
use cg_protocol::phone::{Hello, PhoneMetrics, capability};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub struct ControllerConfig {
    pub endpoint: MuxEndpoint,
    pub backend: Arc<dyn TunnelBackend>,
    pub resolver_addr: SocketAddr,
    pub computer_name: Option<String>,
    pub exit_target: (String, u16),
    pub relisten_delay: Duration,
}

impl ControllerConfig {
    pub fn new(endpoint: MuxEndpoint, backend: Arc<dyn TunnelBackend>) -> Self {
        Self {
            endpoint,
            backend,
            resolver_addr: RESOLVER,
            computer_name: None,
            exit_target: ("www.cloudflare.com".into(), 443),
            relisten_delay: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Connect { serial: Option<String> },
    Disconnect,
}

enum Message {
    Command(Command),
    Shutdown(tokio::sync::oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct Controller {
    messages: mpsc::Sender<Message>,
    snapshot: watch::Receiver<Snapshot>,
}

impl Controller {
    pub fn spawn(config: ControllerConfig) -> Controller {
        let (tx, rx) = mpsc::channel(16);
        let (snapshot_tx, snapshot) = watch::channel(Snapshot::default());
        tokio::spawn(Actor::new(config, snapshot_tx).run(rx));
        Controller { messages: tx, snapshot }
    }

    pub async fn send(&self, command: Command) -> Result<()> {
        self.messages
            .send(Message::Command(command))
            .await
            .map_err(|_| CoreError::Tunnel("CellGlide 服务正在退出".into()))
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    /// Tears the tunnel down and stops the state machine.
    pub async fn shutdown(&self) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.messages.send(Message::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// Events from a session's helper tasks, tagged with the session they belong to.
enum Event {
    Ended(u64),
    DnsChanged(u64),
    Metrics(u64, Box<PhoneMetrics>),
}

/// What one established session holds. Dropping it stops relays and helper tasks;
/// the tunnel itself is stopped explicitly first (see `teardown`).
struct Active {
    device: Device,
    hello: Hello,
    phone_dns: bool,
    tunnel: Option<Box<dyn RunningTunnel>>,
    _forwarder: Forwarder,
    _resolver: Option<Resolver>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

enum Choice {
    Waiting,
    Choose,
    Selected(Device),
}

/// A phone chosen by serial is never replaced; otherwise only a single phone is used.
fn choose(devices: &[Device], chosen: Option<&str>) -> Choice {
    if let Some(serial) = chosen {
        return devices.iter().find(|d| d.serial == serial).cloned().map_or(Choice::Waiting, Choice::Selected);
    }
    match devices {
        [] => Choice::Waiting,
        [one] => Choice::Selected(one.clone()),
        _ => Choice::Choose,
    }
}

fn summary(m: &PhoneMetrics) -> MetricsSummary {
    MetricsSummary {
        carrier: m.carrier.clone(),
        radio: m.radio.clone(),
        download: m.download,
        upload: m.upload,
        battery: m.battery,
        charging: m.charging,
    }
}

struct Actor {
    config: ControllerConfig,
    snapshot: watch::Sender<Snapshot>,
    mux_events: Option<mpsc::Receiver<MuxEvent>>,
    next_listen: Instant,
    mux_up: bool,
    devices: Vec<Device>,
    wants: bool,
    chosen: Option<String>,
    connecting: bool,
    active: Option<Active>,
    last_error: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
    generation: u64,
    metrics: Option<MetricsSummary>,
}

impl Actor {
    fn new(config: ControllerConfig, snapshot: watch::Sender<Snapshot>) -> Self {
        Self {
            config,
            snapshot,
            mux_events: None,
            next_listen: Instant::now(),
            mux_up: true,
            devices: Vec::new(),
            wants: false,
            chosen: None,
            connecting: false,
            active: None,
            last_error: None,
            failures: 0,
            retry_at: None,
            generation: 0,
            metrics: None,
        }
    }

    async fn run(mut self, mut messages: mpsc::Receiver<Message>) {
        let (events_tx, mut events) = mpsc::channel::<Event>(64);
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                message = messages.recv() => match message {
                    Some(Message::Command(command)) => self.command(command).await,
                    Some(Message::Shutdown(done)) => {
                        self.teardown().await;
                        self.publish();
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.teardown().await;
                        return;
                    }
                },
                event = async { self.mux_events.as_mut().unwrap().recv().await }, if self.mux_events.is_some() => {
                    self.mux_event(event).await;
                }
                Some(event) = events.recv() => self.session_event(event).await,
                _ = tick.tick() => self.tick(&events_tx).await,
            }
            self.publish();
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Connect { serial } => {
                if serial.is_some() && serial != self.chosen {
                    self.teardown().await;
                }
                self.wants = true;
                if serial.is_some() {
                    self.chosen = serial;
                }
                self.failures = 0;
                self.last_error = None;
                self.retry_at = Some(Instant::now());
            }
            Command::Disconnect => {
                self.wants = false;
                self.retry_at = None;
                self.last_error = None;
                self.teardown().await;
            }
        }
    }

    async fn mux_event(&mut self, event: Option<MuxEvent>) {
        match event {
            Some(MuxEvent::Attached(device)) => {
                if !self.devices.iter().any(|d| d.mux_id == device.mux_id) {
                    self.devices.push(device);
                    self.devices.sort_by(|a, b| a.serial.cmp(&b.serial));
                }
                self.retry_soon();
            }
            Some(MuxEvent::Detached(id)) => {
                self.devices.retain(|d| d.mux_id != id);
                if self.active.as_ref().is_some_and(|a| a.device.mux_id == id) {
                    self.teardown().await;
                    self.last_error = Some(CoreError::PhoneGone.to_string());
                }
            }
            Some(MuxEvent::Other) => {}
            None => {
                // usbmuxd went away: treat every phone as gone and listen again later.
                tracing::warn!("usbmuxd connection closed");
                self.mux_events = None;
                self.devices.clear();
                self.teardown().await;
                self.next_listen = Instant::now() + self.config.relisten_delay;
            }
        }
    }

    async fn session_event(&mut self, event: Event) {
        match event {
            Event::Ended(g) | Event::DnsChanged(g) if g == self.generation && self.active.is_some() => {
                tracing::info!("phone session changed; rebuilding");
                self.teardown().await;
                self.retry_soon();
            }
            Event::Metrics(g, m) if g == self.generation => self.metrics = Some(summary(&m)),
            _ => {}
        }
    }

    async fn tick(&mut self, events: &mpsc::Sender<Event>) {
        let now = Instant::now();
        if self.mux_events.is_none() && now >= self.next_listen {
            self.relisten().await;
        }
        if self.active.as_ref().and_then(|a| a.tunnel.as_ref()).is_some_and(|t| t.has_exited()) {
            tracing::warn!("hev exited; tearing the tunnel down");
            self.teardown().await;
            self.last_error = Some("流量转发意外停止，正在重新连接".into());
            self.retry_soon();
        }
        if self.wants && self.active.is_none() && self.mux_events.is_some() && self.retry_at.is_some_and(|t| now >= t) {
            self.attempt(events).await;
        }
    }

    async fn relisten(&mut self) {
        let listened = async {
            let events = mux::listen(&self.config.endpoint).await?;
            let devices = mux::list_devices(&self.config.endpoint).await?;
            Ok::<_, CoreError>((events, devices))
        }
        .await;
        match listened {
            Ok((events, devices)) => {
                self.mux_events = Some(events);
                self.devices = devices;
                self.mux_up = true;
                self.retry_soon();
            }
            Err(error) => {
                tracing::debug!("usbmuxd not reachable: {error}");
                self.mux_up = false;
                self.next_listen = Instant::now() + self.config.relisten_delay;
            }
        }
    }

    fn retry_soon(&mut self) {
        if self.wants && self.active.is_none() {
            self.retry_at = Some(Instant::now());
        }
    }

    async fn attempt(&mut self, events: &mpsc::Sender<Event>) {
        let device = match choose(&self.devices, self.chosen.as_deref()) {
            Choice::Selected(device) => device,
            Choice::Waiting | Choice::Choose => {
                self.retry_at = None; // an attach or a choice schedules the next try
                return;
            }
        };
        self.connecting = true;
        self.publish();
        let result = self.establish(device, events).await;
        self.connecting = false;
        match result {
            Ok(active) => {
                self.active = Some(active);
                self.failures = 0;
                self.last_error = None;
                self.retry_at = None;
            }
            Err(error) => {
                tracing::warn!("connect failed: {error}");
                self.failures = (self.failures + 1).min(4);
                self.last_error = Some(error.to_string());
                let wait = Duration::from_secs((1u64 << self.failures).min(30));
                self.retry_at = Some(Instant::now() + wait);
            }
        }
    }

    async fn establish(&mut self, device: Device, events: &mpsc::Sender<Event>) -> Result<Active> {
        let phone = Phone { endpoint: self.config.endpoint.clone(), device: device.clone() };
        let hello = phone::read_hello(&phone).await?;
        hello.validate()?;
        if !hello.has(capability::UDP_OVER_USB) {
            return Err(CoreError::PhoneTooOld);
        }
        self.generation += 1;
        let generation = self.generation;
        let mut tasks = Vec::new();

        if hello.has(capability::SESSION_WATCH) {
            let ended: Ended = phone::watch(&phone, &hello.session_id, self.config.computer_name.as_deref()).await?;
            let tx = events.clone();
            tasks.push(tokio::spawn(async move {
                ended.await;
                let _ = tx.send(Event::Ended(generation)).await;
            }));
        }
        let forwarder = Forwarder::start(phone.clone()).await?;
        let (host, port) = &self.config.exit_target;
        if !probe_exit(forwarder.port(), host, *port).await {
            tasks.iter().for_each(JoinHandle::abort);
            return Err(CoreError::NoExit);
        }
        let phone_dns = hello.dns_enabled() && hello.has(capability::DNS_FORWARD);
        let resolver = if phone_dns {
            let upstream = Arc::new(DohUpstream::new(phone.clone()));
            match Resolver::start(self.config.resolver_addr, upstream).await {
                Ok(resolver) => Some(resolver),
                Err(error) => {
                    tasks.iter().for_each(JoinHandle::abort);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let plan = TunnelPlan {
            socks_port: forwarder.port(),
            dns: match &resolver {
                Some(r) => DnsPlan::Resolver(r.local_addr()),
                None => DnsPlan::Servers(FALLBACK_DNS.to_vec()),
            },
        };
        let backend = self.config.backend.clone();
        let started = tokio::task::spawn_blocking(move || backend.start(&plan)).await;
        let tunnel = match started {
            Ok(Ok(tunnel)) => tunnel,
            Ok(Err(error)) => {
                tasks.iter().for_each(JoinHandle::abort);
                return Err(CoreError::Tunnel(error.0));
            }
            Err(join) => {
                tasks.iter().for_each(JoinHandle::abort);
                return Err(CoreError::Tunnel(join.to_string()));
            }
        };

        tasks.push(spawn_metrics(phone.clone(), generation, events.clone()));
        tasks.push(spawn_status(phone, generation, hello.dns_enabled(), events.clone()));
        self.metrics = None;
        Ok(Active { device, hello, phone_dns, tunnel: Some(tunnel), _forwarder: forwarder, _resolver: resolver, tasks })
    }

    /// Network first: the tunnel goes before the relays it depends on.
    async fn teardown(&mut self) {
        let Some(mut active) = self.active.take() else { return };
        if let Some(tunnel) = active.tunnel.take() {
            match tokio::task::spawn_blocking(move || tunnel.stop()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::error!("stopping tunnel: {error}"),
                Err(error) => tracing::error!("stopping tunnel: {error}"),
            }
        }
        drop(active);
        self.metrics = None;
    }

    fn state(&self) -> ConnState {
        if !self.mux_up {
            return ConnState::NoUsbService;
        }
        if let Some(active) = &self.active {
            return ConnState::Connected { phone_version: active.hello.version.clone(), phone_dns: active.phone_dns };
        }
        if self.connecting {
            return ConnState::Connecting;
        }
        if self.devices.is_empty() {
            return ConnState::NoPhone;
        }
        if self.wants {
            if let Choice::Choose = choose(&self.devices, self.chosen.as_deref()) {
                return ConnState::ChoosePhone;
            }
            return match &self.last_error {
                Some(reason) => ConnState::Retrying { reason: reason.clone() },
                None => ConnState::Connecting,
            };
        }
        ConnState::Idle
    }

    fn publish(&self) {
        let next = Snapshot {
            state: self.state(),
            devices: self.devices.iter().map(|d| d.serial.clone()).collect(),
            selected: self.active.as_ref().map(|a| a.device.serial.clone()).or_else(|| self.chosen.clone()),
            wants_connection: self.wants,
            metrics: self.metrics.clone(),
        };
        self.snapshot.send_if_modified(|current| {
            let changed = *current != next;
            *current = next;
            changed
        });
    }
}

fn spawn_metrics(phone: Phone, generation: u64, events: mpsc::Sender<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (tx, mut rx) = mpsc::channel(4);
            let forward = async {
                while let Some(metrics) = rx.recv().await {
                    if events.send(Event::Metrics(generation, Box::new(metrics))).await.is_err() {
                        return;
                    }
                }
            };
            // Both stop together when the session's tasks are aborted.
            tokio::select! {
                _ = phone::metrics_stream(phone.clone(), tx) => {}
                _ = forward => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn spawn_status(phone: Phone, generation: u64, dns_enabled: bool, events: mpsc::Sender<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel(4);
        let watch_dns = async {
            while let Some(status) = rx.recv().await {
                if status.dns_enabled != dns_enabled {
                    let _ = events.send(Event::DnsChanged(generation)).await;
                    return;
                }
            }
        };
        tokio::select! {
            _ = phone::status_stream(phone, tx) => {}
            _ = watch_dns => {}
        }
    })
}
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p cg-core controller`
Expected: 12 个测试全部 `ok`。若 `usbmuxd_restart_tears_down_and_relistens` 偶发失败，检查 `relisten` 是否在 `drop_listeners` 后重新注册了 Listen（FakeMux 的 `drop_listeners` 只清空当时已有的监听者）。

- [ ] **Step 6: 跑一遍 cg-core 全部测试（macOS 与 Docker）**

Run: `cargo test -p cg-core && scripts/linux-test.sh -p cg-core`
Expected: 全部 `ok`。

- [ ] **Step 7: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-core && git commit -m "cg-core: connection state machine

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12: hev-socks5-tunnel 静态库（`hev-sys`）

参照 `CellProxy:Scripts/build-hev.sh`：同一版本 2.18.0，`make static` 后把 hev 自己的库和三个第三方库一起链接。

**Files:**
- Create: `crates/hev-sys/Cargo.toml`, `crates/hev-sys/build.rs`, `crates/hev-sys/src/lib.rs`, `.gitmodules`（由 git 生成）, 子模块 `crates/hev-sys/hev`

**Interfaces:**
- Produces（仅 `target_os = "linux"`）：
  - `pub unsafe fn hev_socks5_tunnel_main_from_str(config: *const u8, config_len: u32, tun_fd: i32) -> i32`（阻塞，直到 quit 或出错）
  - `pub unsafe fn hev_socks5_tunnel_quit()`

- [ ] **Step 1: 加子模块并固定在 2.18.0**

```bash
git submodule add https://github.com/heiher/hev-socks5-tunnel.git crates/hev-sys/hev
git -C crates/hev-sys/hev checkout 2.18.0
git -C crates/hev-sys/hev submodule update --init --recursive
grep -n "hev_socks5_tunnel_main_from_str\|hev_socks5_tunnel_quit" crates/hev-sys/hev/src/hev-main.h
```

Expected: 头文件里有 `int hev_socks5_tunnel_main_from_str (const unsigned char *config_str, unsigned int config_len, int tun_fd);` 与 `void hev_socks5_tunnel_quit (void);`。若签名不同，以头文件为准修改 Step 3 的声明。

- [ ] **Step 2: Cargo.toml 与 build.rs**

`crates/hev-sys/Cargo.toml`：

```toml
[package]
name = "hev-sys"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true
links = "hev-socks5-tunnel"

[build-dependencies]
cc.workspace = true
```

`crates/hev-sys/build.rs`：

```rust
//! Builds hev-socks5-tunnel 2.18.0 (MIT) from the submodule as static libraries.
//! Only Linux links it in phase 0; other targets build nothing.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=hev/src");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("hev");
    // Build a copy so the submodule stays clean.
    if !out.exists() {
        let status = Command::new("cp").arg("-a").arg(manifest.join("hev")).arg(&out).status().expect("cp");
        assert!(status.success(), "copying hev sources failed");
    }
    let compiler = cc::Build::new().get_compiler();
    let jobs = env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());
    let status = Command::new("make")
        .arg("-C")
        .arg(&out)
        .arg("static")
        .arg(format!("-j{jobs}"))
        .arg(format!("CC={}", compiler.path().display()))
        .status()
        .expect("make");
    assert!(status.success(), "building hev-socks5-tunnel failed");
    for dir in ["bin", "third-part/lwip/bin", "third-part/yaml/bin", "third-part/hev-task-system/bin"] {
        println!("cargo:rustc-link-search=native={}", out.join(dir).display());
    }
    for lib in ["hev-socks5-tunnel", "lwip", "yaml", "hev-task-system"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
```

- [ ] **Step 3: 写声明和失败的测试**

`crates/hev-sys/src/lib.rs`：

```rust
//! FFI to hev-socks5-tunnel. One tunnel per process: `quit` stops the running one.

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// Runs the tunnel on `tun_fd` with a YAML config; blocks until `quit` or an error.
    pub fn hev_socks5_tunnel_main_from_str(config: *const u8, config_len: u32, tun_fd: i32) -> i32;
    pub fn hev_socks5_tunnel_quit();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn rejects_a_broken_config_without_blocking() {
        let config = b"tunnel: [";
        let result = unsafe { super::hev_socks5_tunnel_main_from_str(config.as_ptr(), config.len() as u32, -1) };
        assert_ne!(result, 0);
    }
}
```

- [ ] **Step 4: 在 Linux 容器里编译并运行**

Run: `scripts/linux-test.sh -p hev-sys`
Expected: `rejects_a_broken_config_without_blocking ... ok`。若链接报缺符号（如 `pthread_*`、`clock_gettime`），在 build.rs 末尾加 `println!("cargo:rustc-link-lib=pthread");` 后重跑。

- [ ] **Step 5: macOS 上确认不受影响**

Run: `cargo build -p hev-sys`
Expected: 成功（非 Linux 目标什么也不编）。

- [ ] **Step 6: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add .gitmodules crates/hev-sys && git commit -m "hev-sys: static hev-socks5-tunnel 2.18.0 for Linux

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Linux 隧道后端（`cg-platform`）

一次完成：建 `cellglide0` → `ip` 配地址和路由 → hev 线程 → resolved 单网卡 DNS；任何一步失败都回滚。停止顺序：`RevertLink` → 停 hev → 关 fd（内核随之删除网卡和路由）。

**Files:**
- Create: `crates/cg-platform/Cargo.toml`, `crates/cg-platform/src/lib.rs`, `crates/cg-platform/src/ip.rs`, `crates/cg-platform/src/resolved.rs`, `crates/cg-platform/src/tun.rs`, `crates/cg-platform/src/hev.rs`, `crates/cg-platform/src/backend.rs`, `crates/cg-platform/examples/hold_tunnel.rs`, `crates/cg-platform/tests/privileged.rs`
- Modify: `.github/workflows/ci.yml`（加 `privileged` 任务）

**Interfaces:**
- Consumes: `cg_core::tunnel::{TunnelBackend, RunningTunnel, TunnelPlan, DnsPlan, TunnelError, INTERFACE_NAME, ADDRESS_V4, ADDRESS_V6, MTU, IPV4_ROUTES, IPV6_ROUTES, hev_config}`；`hev_sys`
- Produces:
  - `cg_platform::LinuxBackend`（unit struct，`impl TunnelBackend`，仅 Linux）
  - 纯函数（全平台可测）：`ip::link_commands(name) -> Vec<Vec<String>>`、`ip::ipv6_commands(name) -> Vec<Vec<String>>`、`ip::route_commands(name) -> Vec<Vec<String>>`、`resolved::dns_entries(&DnsPlan) -> Vec<(i32, Vec<u8>, u16, String)>`

- [ ] **Step 1: Cargo.toml 与 lib.rs**

```toml
[package]
name = "cg-platform"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
cg-core.workspace = true
tracing.workspace = true

[target.'cfg(target_os = "linux")'.dependencies]
hev-sys.workspace = true
libc.workspace = true
zbus.workspace = true
```

`crates/cg-platform/src/lib.rs`：

```rust
//! Platform tunnel backends. Phase 0 implements Linux only.

pub mod ip;
pub mod resolved;

#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod hev;
#[cfg(target_os = "linux")]
mod tun;

#[cfg(target_os = "linux")]
pub use backend::LinuxBackend;
```

注意：`resolved.rs` 里与 D-Bus 相关的部分用 `#[cfg(target_os = "linux")]` 包住，纯函数部分全平台编译。

- [ ] **Step 2: 写失败的测试（纯函数）**

`crates/cg-platform/src/ip.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn joined(commands: Vec<Vec<String>>) -> Vec<String> {
        commands.into_iter().map(|c| c.join(" ")).collect()
    }

    #[test]
    fn commands_match_mac_tunnel() {
        assert_eq!(
            joined(link_commands("cellglide0")),
            ["link set dev cellglide0 mtu 1500 up", "addr add 198.18.0.1/32 dev cellglide0"]
        );
        assert_eq!(
            joined(ipv6_commands("cellglide0")),
            [
                "-6 addr add fd6c:6779:6465::1/128 dev cellglide0",
                "-6 route add ::/1 dev cellglide0",
                "-6 route add 8000::/1 dev cellglide0"
            ]
        );
        assert_eq!(
            joined(route_commands("cellglide0")),
            ["route add 0.0.0.0/1 dev cellglide0", "route add 128.0.0.0/1 dev cellglide0"]
        );
    }
}
```

`crates/cg-platform/src/resolved.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::tunnel::{DnsPlan, FALLBACK_DNS};

    #[test]
    fn resolver_plan_carries_port() {
        let entries = dns_entries(&DnsPlan::Resolver("127.0.0.1:1053".parse().unwrap()));
        assert_eq!(entries, vec![(2, vec![127, 0, 0, 1], 1053, String::new())]);
    }

    #[test]
    fn public_servers_use_default_port() {
        let entries = dns_entries(&DnsPlan::Servers(FALLBACK_DNS.to_vec()));
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].0, 10, "AF_INET6 on Linux");
        assert_eq!(entries[0].1.len(), 16);
        assert_eq!(entries[2], (2, vec![1, 1, 1, 1], 0, String::new()));
    }
}
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-platform`
Expected: 编译失败。

- [ ] **Step 4: 实现 `ip.rs`**

```rust
//! Interface addresses and routes through iproute2's `ip`, as the Mac helper uses
//! `ifconfig` and `route`. Everything here belongs to our interface only.

use cg_core::tunnel::{ADDRESS_V4, ADDRESS_V6, IPV4_ROUTES, IPV6_ROUTES, MTU, TunnelError};
use std::process::Command;

fn args(text: &str) -> Vec<String> {
    text.split(' ').map(String::from).collect()
}

pub fn link_commands(name: &str) -> Vec<Vec<String>> {
    vec![args(&format!("link set dev {name} mtu {MTU} up")), args(&format!("addr add {ADDRESS_V4}/32 dev {name}"))]
}

/// Best effort: hosts with IPv6 disabled refuse these, and the tunnel still works for IPv4.
pub fn ipv6_commands(name: &str) -> Vec<Vec<String>> {
    let mut commands = vec![args(&format!("-6 addr add {ADDRESS_V6}/128 dev {name}"))];
    commands.extend(IPV6_ROUTES.iter().map(|r| args(&format!("-6 route add {r} dev {name}"))));
    commands
}

pub fn route_commands(name: &str) -> Vec<Vec<String>> {
    IPV4_ROUTES.iter().map(|r| args(&format!("route add {r} dev {name}"))).collect()
}

pub fn run(arguments: &[String]) -> Result<(), TunnelError> {
    let output = Command::new("ip")
        .args(arguments)
        .output()
        .map_err(|e| TunnelError(format!("无法运行 ip 命令：{e}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(TunnelError(format!(
        "ip {} 失败：{}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
```

- [ ] **Step 5: 实现 `resolved.rs`**

```rust
//! Per-link DNS through systemd-resolved. Only our link is ever touched; when the
//! link disappears resolved forgets its settings, so a crash leaves nothing behind.

use cg_core::tunnel::DnsPlan;
use std::net::IpAddr;

// resolved runs on Linux; these are Linux's address family numbers.
const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;

/// The `a(iayqs)` argument of SetLinkDNSEx: family, address bytes, port (0 = 53), server name.
pub fn dns_entries(plan: &DnsPlan) -> Vec<(i32, Vec<u8>, u16, String)> {
    let entry = |ip: IpAddr, port: u16| match ip {
        IpAddr::V4(v4) => (AF_INET, v4.octets().to_vec(), port, String::new()),
        IpAddr::V6(v6) => (AF_INET6, v6.octets().to_vec(), port, String::new()),
    };
    match plan {
        DnsPlan::Resolver(addr) => vec![entry(addr.ip(), addr.port())],
        DnsPlan::Servers(ips) => ips.iter().map(|ip| entry(*ip, 0)).collect(),
    }
}

#[cfg(target_os = "linux")]
mod bus {
    use super::dns_entries;
    use cg_core::tunnel::{DnsPlan, TunnelError};

    #[zbus::proxy(
        interface = "org.freedesktop.resolve1.Manager",
        default_service = "org.freedesktop.resolve1",
        default_path = "/org/freedesktop/resolve1"
    )]
    trait Resolve1Manager {
        #[zbus(name = "SetLinkDNSEx")]
        fn set_link_dns_ex(&self, ifindex: i32, addresses: &[(i32, Vec<u8>, u16, String)]) -> zbus::Result<()>;
        fn set_link_domains(&self, ifindex: i32, domains: &[(&str, bool)]) -> zbus::Result<()>;
        fn set_link_default_route(&self, ifindex: i32, enable: bool) -> zbus::Result<()>;
        fn revert_link(&self, ifindex: i32) -> zbus::Result<()>;
    }

    fn manager() -> Result<Resolve1ManagerProxyBlocking<'static>, TunnelError> {
        let connection = zbus::blocking::Connection::system()
            .map_err(|e| TunnelError(format!("连不上系统 D-Bus：{e}")))?;
        Resolve1ManagerProxyBlocking::new(&connection)
            .map_err(|e| TunnelError(format!("找不到 systemd-resolved：{e}")))
    }

    /// All lookups go to our link: its DNS servers, the routing-only domain "~." and default route.
    pub fn apply(ifindex: i32, plan: &DnsPlan) -> Result<(), TunnelError> {
        let manager = manager()?;
        let failed = |e: zbus::Error| TunnelError(format!("systemd-resolved 拒绝了 DNS 设置：{e}"));
        manager.set_link_dns_ex(ifindex, &dns_entries(plan)).map_err(failed)?;
        manager.set_link_domains(ifindex, &[(".", true)]).map_err(failed)?;
        manager.set_link_default_route(ifindex, true).map_err(failed)?;
        Ok(())
    }

    pub fn revert(ifindex: i32) -> Result<(), TunnelError> {
        manager()?.revert_link(ifindex).map_err(|e| TunnelError(format!("撤销 DNS 设置失败：{e}")))
    }
}

#[cfg(target_os = "linux")]
pub use bus::{apply, revert};
```

若编译时 `Resolve1ManagerProxyBlocking::new` 的参数要求 `&Connection` 以外的形式，按 zbus 5.19 文档（docs.rs/zbus/5.19.0，`blocking::Proxy` 生成代码）调整；接口名和方法签名保持不变。

- [ ] **Step 6: 运行纯函数测试**

Run: `cargo test -p cg-platform`
Expected: macOS 上 3 个测试 `ok`。

- [ ] **Step 7: 实现 `tun.rs`、`hev.rs`、`backend.rs`（Linux）**

`crates/cg-platform/src/tun.rs`：

```rust
//! Our TUN device. It exists only while this descriptor is open: closing it (or the
//! process dying) makes the kernel delete the interface and its routes.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

const TUNSETIFF: libc::c_ulong = 0x4004_54ca;

pub fn open(name: &str) -> io::Result<OwnedFd> {
    let path = CString::new("/dev/net/tun").unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(name.bytes()) {
        *slot = byte as libc::c_char;
    }
    request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
    let result = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&fd), TUNSETIFF as _, &request) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub fn index(name: &str) -> io::Result<i32> {
    let name = CString::new(name).unwrap();
    match unsafe { libc::if_nametoindex(name.as_ptr()) } {
        0 => Err(io::Error::last_os_error()),
        index => Ok(index as i32),
    }
}
```

`crates/cg-platform/src/hev.rs`：

```rust
//! hev-socks5-tunnel on its own thread. hev keeps global state, so there is at most
//! one per process.

use cg_core::tunnel::TunnelError;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long hev gets to fail on its config before the tunnel counts as up.
const STARTUP_GRACE: Duration = Duration::from_millis(300);
const STOP_WAIT: Duration = Duration::from_secs(5);

pub struct Hev {
    thread: Option<JoinHandle<i32>>,
    exited: Arc<AtomicBool>,
}

impl Hev {
    pub fn spawn(config: String, tun_fd: RawFd) -> Result<Hev, TunnelError> {
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        let thread = std::thread::Builder::new()
            .name("hev".into())
            .spawn(move || {
                let code = unsafe { hev_sys::hev_socks5_tunnel_main_from_str(config.as_ptr(), config.len() as u32, tun_fd) };
                flag.store(true, Ordering::SeqCst);
                code
            })
            .map_err(|e| TunnelError(format!("无法启动转发线程：{e}")))?;
        std::thread::sleep(STARTUP_GRACE);
        let hev = Hev { thread: Some(thread), exited };
        if hev.has_exited() {
            let code = hev.join();
            return Err(TunnelError(format!("流量转发启动失败（hev 返回 {code}）")));
        }
        Ok(hev)
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn join(mut self) -> i32 {
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or(-1)
    }

    /// Asks hev to quit until it does (an early quit can land before hev is listening for it).
    pub fn stop(self) {
        let deadline = Instant::now() + STOP_WAIT;
        while !self.has_exited() && Instant::now() < deadline {
            unsafe { hev_sys::hev_socks5_tunnel_quit() };
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.has_exited() {
            self.join();
        } else {
            tracing::error!("hev did not quit within {STOP_WAIT:?}; leaving its thread");
        }
    }
}
```

`crates/cg-platform/src/backend.rs`：

```rust
use crate::hev::Hev;
use crate::{ip, resolved, tun};
use cg_core::tunnel::{INTERFACE_NAME, RunningTunnel, TunnelBackend, TunnelError, TunnelPlan, hev_config};
use std::os::fd::{AsRawFd, OwnedFd};

/// /dev/net/tun + ip + systemd-resolved + hev.
pub struct LinuxBackend;

struct Running {
    index: i32,
    hev: Option<Hev>,
    /// Dropped last: closing it removes the interface and its routes.
    fd: Option<OwnedFd>,
}

impl TunnelBackend for LinuxBackend {
    fn start(&self, plan: &TunnelPlan) -> Result<Box<dyn RunningTunnel>, TunnelError> {
        let fd = tun::open(INTERFACE_NAME).map_err(|e| match e.raw_os_error() {
            Some(libc::EBUSY) => TunnelError(format!("{INTERFACE_NAME} 已被占用，是否还有另一个 CellGlide 在运行？")),
            _ => TunnelError(format!("无法创建虚拟网卡 {INTERFACE_NAME}：{e}")),
        })?;
        // From here on an early return drops `fd`, which undoes the interface and routes.
        for command in ip::link_commands(INTERFACE_NAME) {
            ip::run(&command)?;
        }
        if let Err(error) = ip::ipv6_commands(INTERFACE_NAME).iter().try_for_each(|c| ip::run(c)) {
            tracing::warn!("IPv6 not set up on {INTERFACE_NAME}: {error}");
        }
        for command in ip::route_commands(INTERFACE_NAME) {
            ip::run(&command)?;
        }
        let index = tun::index(INTERFACE_NAME).map_err(|e| TunnelError(format!("找不到 {INTERFACE_NAME}：{e}")))?;
        let hev = Hev::spawn(hev_config(plan.socks_port), fd.as_raw_fd())?;
        if let Err(error) = resolved::apply(index, &plan.dns) {
            let _ = resolved::revert(index);
            hev.stop();
            return Err(error);
        }
        Ok(Box::new(Running { index, hev: Some(hev), fd: Some(fd) }))
    }
}

impl RunningTunnel for Running {
    fn stop(mut self: Box<Self>) -> Result<(), TunnelError> {
        let reverted = resolved::revert(self.index);
        if let Some(hev) = self.hev.take() {
            hev.stop();
        }
        drop(self.fd.take());
        reverted
    }

    fn has_exited(&self) -> bool {
        self.hev.as_ref().is_none_or(Hev::has_exited)
    }
}
```

- [ ] **Step 8: 在容器里编译 Linux 部分**

Run: `scripts/linux-test.sh -p cg-platform`
Expected: 编译通过，3 个纯函数测试 `ok`（容器里没有 resolved，特权测试在 CI 跑）。

- [ ] **Step 9: 写特权测试与辅助示例**

`crates/cg-platform/examples/hold_tunnel.rs`：

```rust
//! Brings the tunnel up and waits forever; the privileged tests SIGKILL it.

#[cfg(target_os = "linux")]
fn main() {
    use cg_core::tunnel::{DnsPlan, RESOLVER, TunnelBackend, TunnelPlan};
    let socks = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let plan = TunnelPlan { socks_port: socks.local_addr().unwrap().port(), dns: DnsPlan::Resolver(RESOLVER) };
    let _tunnel = cg_platform::LinuxBackend.start(&plan).expect("tunnel up");
    println!("up");
    loop {
        std::thread::park();
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
```

`crates/cg-platform/tests/privileged.rs`：

```rust
//! Needs root and systemd-resolved. Run: sudo -E cargo test -p cg-platform --test privileged -- --ignored --test-threads=1
#![cfg(target_os = "linux")]

use cg_core::tunnel::{DnsPlan, INTERFACE_NAME, RESOLVER, TunnelBackend, TunnelPlan};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn snapshot() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/network-snapshot.sh");
    let out = Command::new(script).arg(INTERFACE_NAME).output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn sh(command: &str) -> String {
    let out = Command::new("sh").arg("-c").arg(command).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn link_exists() -> bool {
    Path::new("/sys/class/net").join(INTERFACE_NAME).exists()
}

fn plan() -> TunnelPlan {
    TunnelPlan { socks_port: 1080, dns: DnsPlan::Resolver(RESOLVER) }
}

#[test]
#[ignore]
fn tunnel_up_and_down_touches_only_our_link() {
    let before = snapshot();
    let tunnel = cg_platform::LinuxBackend.start(&plan()).unwrap();
    assert!(sh("ip route get 1.1.1.1").contains(&format!("dev {INTERFACE_NAME}")));
    assert!(sh(&format!("resolvectl dns {INTERFACE_NAME}")).contains(&RESOLVER.to_string()));
    assert!(sh(&format!("resolvectl domain {INTERFACE_NAME}")).contains("~."));
    assert_eq!(snapshot(), before, "other links changed while up");
    assert!(!tunnel.has_exited());
    tunnel.stop().unwrap();
    assert!(!link_exists());
    assert_eq!(snapshot(), before, "other links changed after stop");
}

#[test]
#[ignore]
fn second_tunnel_is_refused() {
    let first = cg_platform::LinuxBackend.start(&plan()).unwrap();
    let second = cg_platform::LinuxBackend.start(&plan());
    assert!(second.is_err());
    first.stop().unwrap();
    assert!(!link_exists());
}

#[test]
#[ignore]
fn killed_holder_leaves_nothing() {
    let before = snapshot();
    let example: PathBuf = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("examples/hold_tunnel");
    let mut child = Command::new(example).spawn().expect("build examples first: cargo build -p cg-platform --examples");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !link_exists() {
        assert!(Instant::now() < deadline, "tunnel never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill().unwrap(); // SIGKILL
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!link_exists());
    assert!(!sh("resolvectl dns").contains(&RESOLVER.to_string()));
    assert_eq!(snapshot(), before);
}
```

注意 `second_tunnel_is_refused` 依赖 hev 的全局状态：第二次 `start` 在 `tun::open` 处就因 EBUSY 失败，不会碰到 hev。

- [ ] **Step 10: CI 特权任务**

在 `.github/workflows/ci.yml` 的 `jobs:` 下追加：

```yaml
  privileged:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v5
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo build -p cg-platform --examples --tests
      - run: sudo -E env "PATH=$PATH" cargo test -p cg-platform --test privileged -- --ignored --test-threads=1
```

- [ ] **Step 11: 推送并确认 CI 全绿**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-platform .github/workflows/ci.yml && git commit -m "cg-platform: Linux tunnel backend with privileged tests

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push && sleep 5 && gh run watch --exit-status $(gh run list --workflow ci --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: `linux`、`windows-check`、`resolved-probe`、`privileged` 四个任务全部通过。`windows-check` 若因 `cg-platform` 在 Windows 上没有内容而报 unused 警告，属于 `cargo check` 的正常输出，不影响通过。

---
### Task 14: root 服务（`cg-service`）

**Files:**
- Create: `crates/cg-service/Cargo.toml`, `crates/cg-service/src/main.rs`, `crates/cg-service/src/server.rs`, `crates/cg-service/src/auth.rs`, `crates/cg-service/src/doctor.rs`

**Interfaces:**
- Consumes: `cg_core::controller::{Controller, ControllerConfig, Command}`、`cg_core::mux::{self, MuxEndpoint}`、`cg_core::resolver::BoxFuture`、`cg_ipc::*`、`cg_platform::LinuxBackend`
- Produces:
  - 可执行文件 `cellglide-service`
  - `server::bind(path: &Path, group: &str) -> io::Result<UnixListener>`（删除残留 socket、权限 0660、属组为 `group`）
  - `server::serve(listener, controller: Controller, doctor: Arc<dyn DoctorApi>, policy: auth::Policy)`
  - `pub trait DoctorApi: Send + Sync + 'static { fn run(&self) -> BoxFuture<'_, Vec<Check>>; }`
  - `auth::Policy`：`fn for_test(group_gid: Option<u32>, groups_of: impl Fn(u32) -> Vec<u32> + Send + Sync + 'static) -> Policy`、`fn allows(&self, uid: u32, gid: u32) -> bool`；Linux 上 `fn system(group: &str) -> Policy`
  - `doctor::checks(endpoint: &MuxEndpoint, root: &Path) -> Vec<Check>`（`root` 平时为 `/`，测试时为临时目录）

Linux 的特殊情况：Ubuntu、Fedora 的 usbmuxd 由 udev 在插上 iPhone 时启动、拔掉后退出，所以「连不上 usbmuxd」通常只是「没插手机」。`doctor` 因此检查 usbmuxd 是否**已安装**，而不是是否在运行。

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "cg-service"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[[bin]]
name = "cellglide-service"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
cg-core.workspace = true
cg-ipc.workspace = true
cg-platform.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

# nix does not build on Windows; the service is Unix-only in phase 0.
[target.'cfg(unix)'.dependencies]
nix.workspace = true

[dev-dependencies]
cg-core = { workspace = true, features = ["testing"] }
tempfile.workspace = true
```

- [ ] **Step 2: 写失败的测试**

`crates/cg-service/src/auth.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_group_members_only() {
        let policy = Policy::for_test(Some(500), |uid| if uid == 1001 { vec![20, 500] } else { vec![20] });
        assert!(policy.allows(0, 0));
        assert!(policy.allows(1000, 500), "primary group");
        assert!(policy.allows(1001, 20), "supplementary group");
        assert!(!policy.allows(1002, 20));
        let no_group = Policy::for_test(None, |_| vec![500]);
        assert!(!no_group.allows(1001, 500));
        assert!(no_group.allows(0, 0));
    }
}
```

`crates/cg-service/src/doctor.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::testing::{FakeMux, device};

    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn healthy_machine_passes_everything() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        mux.add_pair_record("aaaa");
        let root = root_with(&["usr/sbin/usbmuxd", "run/systemd/resolve/stub-resolv.conf", "dev/net/tun"]);
        let checks = checks(&mux.endpoint(), root.path()).await;
        assert_eq!(checks.len(), 4);
        assert!(checks.iter().all(|c| c.ok), "{checks:?}");
    }

    #[tokio::test]
    async fn untrusted_phone_and_missing_pieces_have_fixes() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let root = root_with(&[]);
        let checks = checks(&mux.endpoint(), root.path()).await;
        // usbmuxd answers (the fake), so only trust, resolved and tun fail.
        assert!(checks[0].ok);
        let failed: Vec<_> = checks.iter().filter(|c| !c.ok).collect();
        assert_eq!(failed.len(), 3, "{checks:?}");
        assert!(failed.iter().all(|c| c.fix.is_some()));
        assert!(failed.iter().any(|c| c.detail.contains("信任")));
    }

    #[tokio::test]
    async fn no_phone_plugged_is_not_a_usbmuxd_failure() {
        let gone = MuxEndpoint::Unix("/nonexistent/usbmuxd".into());
        let root = root_with(&["usr/sbin/usbmuxd"]);
        let checks = checks(&gone, root.path()).await;
        assert!(checks[0].ok, "installed usbmuxd starts when a phone is plugged in");
        assert!(!checks[1].ok);
        assert!(checks[1].detail.contains("没有"));
    }
}
```

`crates/cg-service/src/server.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cg_core::controller::ControllerConfig;
    use cg_core::testing::{FakeBackend, FakeMux, SharedFake};
    use cg_ipc::client;
    use tokio::io::AsyncWriteExt;

    struct NoDoctor;
    impl DoctorApi for NoDoctor {
        fn run(&self) -> BoxFuture<'_, Vec<Check>> {
            Box::pin(async { vec![Check { name: "x".into(), ok: true, detail: "fine".into(), fix: None }] })
        }
    }

    async fn start(policy: Policy) -> (tempfile::TempDir, std::path::PathBuf, FakeMux) {
        let mux = FakeMux::start().await;
        let controller = Controller::spawn(ControllerConfig::new(mux.endpoint(), Arc::new(SharedFake(FakeBackend::new()))));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = bind(&path, "no-such-group-cellglide-test").unwrap();
        tokio::spawn(serve(listener, controller, Arc::new(NoDoctor), policy));
        (dir, path, mux)
    }

    fn me() -> Policy {
        let uid = nix::unistd::getuid().as_raw();
        Policy::for_test(None, move |_| vec![]).with_extra_uid(uid)
    }

    #[tokio::test]
    async fn answers_status_doctor_and_commands() {
        let (_dir, path, _mux) = start(me()).await;
        assert!(matches!(client::request(&path, &Request::Status).await.unwrap(), Response::Status { .. }));
        assert!(matches!(client::request(&path, &Request::Doctor).await.unwrap(), Response::Doctor { checks } if checks.len() == 1));
        assert_eq!(client::request(&path, &Request::Disconnect).await.unwrap(), Response::Ok);
        let mut updates = client::subscribe(&path).await.unwrap();
        assert!(updates.recv().await.is_some());
    }

    #[tokio::test]
    async fn garbage_line_gets_error_and_server_survives() {
        let (_dir, path, _mux) = start(me()).await;
        let mut raw = tokio::net::UnixStream::connect(&path).await.unwrap();
        raw.write_all(b"not json\n").await.unwrap();
        let mut reader = tokio::io::BufReader::new(raw);
        let line = cg_ipc::read_line(&mut reader).await.unwrap().unwrap();
        assert!(matches!(cg_ipc::from_line::<Response>(&line).unwrap(), Response::Error { .. }));

        let mut flood = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _ = flood.write_all(&vec![b'x'; cg_ipc::MAX_LINE * 2]).await;
        drop(flood);

        assert!(matches!(client::request(&path, &Request::Status).await.unwrap(), Response::Status { .. }));
    }

    #[tokio::test]
    async fn strangers_are_turned_away() {
        if nix::unistd::getuid().is_root() {
            return; // root is always allowed
        }
        let (_dir, path, _mux) = start(Policy::for_test(Some(u32::MAX - 1), |_| vec![])).await;
        let reply = client::request(&path, &Request::Status).await.unwrap();
        assert!(matches!(reply, Response::Error { message } if message.contains("cellglide 组")));
    }

    #[test]
    fn socket_is_group_only() {
        use std::os::unix::fs::PermissionsExt;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        std::fs::write(&path, "stale").unwrap();
        let _listener = bind(&path, "no-such-group-cellglide-test").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o660);
    }
}
```

（`with_extra_uid` 让测试进程本身被放行，见 Step 4 的 `auth.rs`。）

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-service`
Expected: 编译失败。

- [ ] **Step 4: 实现 `auth.rs`**

```rust
//! Who may use the control socket: root and members of the cellglide group.

use std::sync::Arc;

type GroupsOf = Arc<dyn Fn(u32) -> Vec<u32> + Send + Sync>;

#[derive(Clone)]
pub struct Policy {
    group_gid: Option<u32>,
    groups_of: GroupsOf,
    extra_uid: Option<u32>,
}

impl Policy {
    pub fn for_test(group_gid: Option<u32>, groups_of: impl Fn(u32) -> Vec<u32> + Send + Sync + 'static) -> Policy {
        Policy { group_gid, groups_of: Arc::new(groups_of), extra_uid: None }
    }

    /// Also allow this uid (tests run the server as an ordinary user).
    pub fn with_extra_uid(mut self, uid: u32) -> Policy {
        self.extra_uid = Some(uid);
        self
    }

    pub fn allows(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.extra_uid == Some(uid) {
            return true;
        }
        self.group_gid.is_some_and(|group| gid == group || (self.groups_of)(uid).contains(&group))
    }

    #[cfg(target_os = "linux")]
    pub fn system(group: &str) -> Policy {
        use nix::unistd::{Group, Uid, User, getgrouplist};
        use std::ffi::CString;
        let gid = Group::from_name(group).ok().flatten().map(|g| g.gid.as_raw());
        Policy::for_test(gid, |uid| {
            let Ok(Some(user)) = User::from_uid(Uid::from_raw(uid)) else { return Vec::new() };
            let Ok(name) = CString::new(user.name) else { return Vec::new() };
            getgrouplist(&name, user.gid).map(|groups| groups.into_iter().map(|g| g.as_raw()).collect()).unwrap_or_default()
        })
    }
}
```

- [ ] **Step 5: 实现 `doctor.rs`**

```rust
//! Environment checks run by the service (as root), each with a fix the user can run.

use cg_core::mux::{self, MuxEndpoint};
use cg_ipc::Check;
use std::path::Path;

fn check(name: &str, ok: bool, detail: impl Into<String>, fix: Option<&str>) -> Check {
    Check { name: name.into(), ok, detail: detail.into(), fix: if ok { None } else { fix.map(String::from) } }
}

pub async fn checks(endpoint: &MuxEndpoint, root: &Path) -> Vec<Check> {
    let listed = mux::list_devices(endpoint).await;
    let installed = ["usr/sbin/usbmuxd", "usr/bin/usbmuxd"].iter().any(|p| root.join(p).exists());

    let usbmuxd = match &listed {
        Ok(_) => check("usbmuxd", true, "USB 服务正在运行", None),
        Err(_) if installed => check("usbmuxd", true, "USB 服务已安装，插上 iPhone 时会自动启动", None),
        Err(_) => check(
            "usbmuxd",
            false,
            "没有安装 USB 服务 usbmuxd",
            Some("Ubuntu / Debian：sudo apt install usbmuxd；Fedora：sudo dnf install usbmuxd"),
        ),
    };

    let trust = match &listed {
        Ok(devices) if !devices.is_empty() => {
            let mut untrusted = Vec::new();
            for device in devices {
                if !mux::has_pair_record(endpoint, &device.serial).await.unwrap_or(false) {
                    untrusted.push(device.serial.clone());
                }
            }
            if untrusted.is_empty() {
                check("trust", true, format!("{} 部 iPhone 已信任这台电脑", devices.len()), None)
            } else {
                check(
                    "trust",
                    false,
                    format!("iPhone 还没有信任这台电脑：{}", untrusted.join("、")),
                    Some("解锁 iPhone，在弹窗中点「信任」；没有弹窗时重新插拔数据线"),
                )
            }
        }
        _ => check("trust", false, "没有检测到插着的 iPhone", Some("用数据线连接 iPhone 并解锁屏幕")),
    };

    let resolved = check(
        "systemd-resolved",
        root.join("run/systemd/resolve/stub-resolv.conf").exists(),
        "第 0 阶段需要 systemd-resolved 来只给 CellGlide 的网卡设置 DNS",
        Some("sudo systemctl enable --now systemd-resolved"),
    );
    let tun = check(
        "tun",
        root.join("dev/net/tun").exists(),
        "需要内核的 TUN 虚拟网卡支持（/dev/net/tun）",
        Some("sudo modprobe tun"),
    );
    vec![usbmuxd, trust, resolved, tun]
}
```

注意：`resolved` 与 `tun` 两项通过时 `detail` 仍是说明文字，`fix` 为 `None`。

- [ ] **Step 6: 实现 `server.rs`**

```rust
//! The control socket: newline-delimited JSON requests from cellglide and cellglide-gui.

use crate::auth::Policy;
use cg_core::controller::{Command, Controller};
pub use cg_core::resolver::BoxFuture;
use cg_ipc::{Check, Request, Response, from_line, read_line, to_line};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub trait DoctorApi: Send + Sync + 'static {
    fn run(&self) -> BoxFuture<'_, Vec<Check>>;
}

pub fn bind(path: &Path, group: &str) -> io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    if let Ok(Some(g)) = nix::unistd::Group::from_name(group) {
        nix::unistd::chown(path, None, Some(g.gid)).map_err(io::Error::from)?;
    }
    Ok(listener)
}

pub async fn serve(listener: UnixListener, controller: Controller, doctor: Arc<dyn DoctorApi>, policy: Policy) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let allowed = stream.peer_cred().is_ok_and(|cred| policy.allows(cred.uid(), cred.gid()));
        if !allowed {
            // Read the request first so the client never writes into a closed socket.
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), read_line(&mut BufReader::new(r))).await;
                let reply = Response::Error { message: "没有权限：当前用户不在 cellglide 组".into() };
                let _ = w.write_all(&to_line(&reply)).await;
            });
            continue;
        }
        tokio::spawn(client(stream, controller.clone(), doctor.clone()));
    }
}

async fn client(stream: UnixStream, controller: Controller, doctor: Arc<dyn DoctorApi>) {
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);
    loop {
        let line = match read_line(&mut r).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(_) => {
                let _ = w.write_all(&to_line(&Response::Error { message: "消息过长或不完整".into() })).await;
                return;
            }
        };
        let reply = match from_line::<Request>(&line) {
            Err(_) => Response::Error { message: "无法识别的请求".into() },
            Ok(Request::Status) => Response::Status { snapshot: controller.snapshot() },
            Ok(Request::Doctor) => Response::Doctor { checks: doctor.run().await },
            Ok(Request::Connect { serial }) => outcome(controller.send(Command::Connect { serial }).await),
            Ok(Request::Disconnect) => outcome(controller.send(Command::Disconnect).await),
            Ok(Request::Subscribe) => {
                let mut updates = controller.subscribe();
                loop {
                    let snapshot = updates.borrow_and_update().clone();
                    if w.write_all(&to_line(&Response::Status { snapshot })).await.is_err() || updates.changed().await.is_err() {
                        return;
                    }
                }
            }
        };
        if w.write_all(&to_line(&reply)).await.is_err() {
            return;
        }
    }
}

fn outcome(result: cg_core::Result<()>) -> Response {
    match result {
        Ok(()) => Response::Ok,
        Err(error) => Response::Error { message: error.to_string() },
    }
}
```

- [ ] **Step 7: 实现 `main.rs`**

```rust
#[cfg(unix)]
mod auth;
#[cfg(unix)]
mod doctor;
#[cfg(unix)]
mod server;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use cg_core::controller::{Controller, ControllerConfig};
    use cg_core::mux::MuxEndpoint;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::signal::unix::{SignalKind, signal};

    struct SystemDoctor(MuxEndpoint);
    impl server::DoctorApi for SystemDoctor {
        fn run(&self) -> server::BoxFuture<'_, Vec<cg_ipc::Check>> {
            Box::pin(doctor::checks(&self.0, Path::new("/")))
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    anyhow::ensure!(nix::unistd::geteuid().is_root(), "cellglide-service 需要以 root 运行");

    let mut config = ControllerConfig::new(MuxEndpoint::system(), Arc::new(cg_platform::LinuxBackend));
    config.computer_name = nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok());
    let controller = Controller::spawn(config);

    let path = Path::new(cg_ipc::SOCKET_PATH);
    let listener = server::bind(path, cg_ipc::GROUP)?;
    let serving = server::serve(
        listener,
        controller.clone(),
        Arc::new(SystemDoctor(MuxEndpoint::system())),
        auth::Policy::system(cg_ipc::GROUP),
    );
    let mut terminate = signal(SignalKind::terminate())?;
    tracing::info!("cellglide-service ready on {}", path.display());
    tokio::select! {
        _ = serving => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    controller.shutdown().await;
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("cellglide-service 第 0 阶段只支持 Linux")
}
```

在 macOS 上 `auth`、`doctor`、`server` 只被测试用到，`cargo clippy` 可能报 dead_code。在这三个 `mod` 声明上方各加 `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]`。

- [ ] **Step 8: 运行，确认通过**

Run: `cargo test -p cg-service && scripts/linux-test.sh -p cg-service`
Expected: macOS 上 auth 1、doctor 3、server 4 个测试 `ok`；容器里以 root 运行，`strangers_are_turned_away` 直接返回也算通过。

- [ ] **Step 9: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-service && git commit -m "cg-service: root service with control socket and doctor

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: 状态文字与命令行（`cg-ipc::describe`、`cg-cli`）

状态文字放在 `cg-ipc::describe`，命令行和界面共用。

**Files:**
- Create: `crates/cg-ipc/src/describe.rs`, `crates/cg-cli/Cargo.toml`, `crates/cg-cli/src/main.rs`
- Modify: `crates/cg-ipc/src/lib.rs`（加 `pub mod describe;`）

**Interfaces:**
- Consumes: `cg_ipc::{client, Request, Response, Snapshot, ConnState, MetricsSummary, Check}`
- Produces:
  - `describe::state_line(&Snapshot) -> String`、`describe::metrics_line(&MetricsSummary) -> Option<String>`、`describe::speed(f64) -> String`、`describe::checks(&[Check]) -> String`、`describe::toggle_request(&Snapshot) -> Request`
  - 可执行文件 `cellglide`：`status`、`connect [--serial S]`、`disconnect`、`doctor`、`watch`

- [ ] **Step 1: 写失败的测试**

`crates/cg-ipc/src/describe.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn snap(state: ConnState) -> Snapshot {
        Snapshot { state, devices: vec!["00008110-001A".into()], ..Snapshot::default() }
    }

    #[test]
    fn states_read_naturally() {
        assert_eq!(state_line(&snap(ConnState::NoPhone)), "没有连接 iPhone");
        assert!(state_line(&snap(ConnState::Idle)).contains("cellglide connect"));
        assert_eq!(
            state_line(&snap(ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: true })),
            "已连接，电脑正在使用手机网络（手机 CellGlide 2.2.0）"
        );
        assert!(state_line(&snap(ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: false })).contains("公共 DNS"));
        assert!(state_line(&snap(ConnState::Retrying { reason: "手机网络暂时不通".into() })).ends_with("手机网络暂时不通"));
        let choose = state_line(&Snapshot { state: ConnState::ChoosePhone, devices: vec!["a".into(), "b".into()], ..Snapshot::default() });
        assert!(choose.contains("--serial") && choose.contains("a") && choose.contains("b"));
        assert!(state_line(&snap(ConnState::NoUsbService)).contains("usbmuxd"));
    }

    #[test]
    fn speeds_and_metrics() {
        assert_eq!(speed(512.0), "512 B/s");
        assert_eq!(speed(2048.0), "2.0 KB/s");
        assert_eq!(speed(3.5 * 1024.0 * 1024.0), "3.5 MB/s");
        let m = MetricsSummary {
            carrier: Some("中国移动".into()),
            radio: Some("5G SA".into()),
            download: Some(2048.0),
            upload: Some(512.0),
            battery: Some(0.8),
            charging: Some(true),
        };
        assert_eq!(metrics_line(&m).unwrap(), "中国移动 5G SA · ↓ 2.0 KB/s ↑ 512 B/s · 电量 80%（充电中）");
        assert_eq!(metrics_line(&MetricsSummary::default()), None);
    }

    #[test]
    fn toggle_and_checks() {
        let mut s = snap(ConnState::Idle);
        assert_eq!(toggle_request(&s), Request::Connect { serial: None });
        s.wants_connection = true;
        assert_eq!(toggle_request(&s), Request::Disconnect);
        let text = checks(&[
            Check { name: "tun".into(), ok: true, detail: "好".into(), fix: None },
            Check { name: "trust".into(), ok: false, detail: "未信任".into(), fix: Some("点信任".into()) },
        ]);
        assert_eq!(text, "✓ 好\n✗ 未信任\n  修复：点信任\n");
    }
}
```

`lib.rs` 加 `pub mod describe;`。

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p cg-ipc describe`
Expected: 编译失败。

- [ ] **Step 3: 实现 `describe.rs`**

```rust
//! Plain-language Chinese text for states, readings and checks.

use crate::{Check, ConnState, MetricsSummary, Request, Snapshot};
use std::fmt::Write as _;

pub fn state_line(snapshot: &Snapshot) -> String {
    match &snapshot.state {
        ConnState::NoUsbService => "没有检测到 iPhone（USB 服务 usbmuxd 未运行，插上 iPhone 后会自动启动）".into(),
        ConnState::NoPhone => "没有连接 iPhone".into(),
        ConnState::ChoosePhone => format!(
            "插着多部 iPhone，请用 cellglide connect --serial <序列号> 选择：{}",
            snapshot.devices.join("、")
        ),
        ConnState::Idle => "iPhone 已连接，运行 cellglide connect 开始使用手机网络".into(),
        ConnState::Connecting => "正在连接…".into(),
        ConnState::Connected { phone_version, phone_dns: true } => {
            format!("已连接，电脑正在使用手机网络（手机 CellGlide {phone_version}）")
        }
        ConnState::Connected { phone_version, phone_dns: false } => format!(
            "已连接，电脑正在使用手机网络（手机 CellGlide {phone_version}；手机上的 CellGlide DNS 已关闭，使用公共 DNS）"
        ),
        ConnState::Retrying { reason } => format!("连接未成功，稍后自动重试：{reason}"),
    }
}

pub fn speed(bytes_per_second: f64) -> String {
    const KB: f64 = 1024.0;
    if bytes_per_second < KB {
        format!("{} B/s", bytes_per_second.round() as u64)
    } else if bytes_per_second < KB * KB {
        format!("{:.1} KB/s", bytes_per_second / KB)
    } else {
        format!("{:.1} MB/s", bytes_per_second / KB / KB)
    }
}

pub fn metrics_line(m: &MetricsSummary) -> Option<String> {
    let mut parts = Vec::new();
    let network: Vec<&str> = [m.carrier.as_deref(), m.radio.as_deref()].into_iter().flatten().collect();
    if !network.is_empty() {
        parts.push(network.join(" "));
    }
    if let (Some(down), Some(up)) = (m.download, m.upload) {
        parts.push(format!("↓ {} ↑ {}", speed(down), speed(up)));
    }
    if let Some(level) = m.battery {
        let charging = if m.charging == Some(true) { "（充电中）" } else { "" };
        parts.push(format!("电量 {}%{charging}", (level * 100.0).round() as u32));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

pub fn checks(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let _ = writeln!(out, "{} {}", if check.ok { "✓" } else { "✗" }, check.detail);
        if let Some(fix) = &check.fix {
            let _ = writeln!(out, "  修复：{fix}");
        }
    }
    out
}

/// What the connect switch does from this state.
pub fn toggle_request(snapshot: &Snapshot) -> Request {
    if snapshot.wants_connection { Request::Disconnect } else { Request::Connect { serial: None } }
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p cg-ipc describe`
Expected: 3 个测试 `ok`。

- [ ] **Step 5: 命令行**

`crates/cg-cli/Cargo.toml`：

```toml
[package]
name = "cg-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[[bin]]
name = "cellglide"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
cg-ipc.workspace = true
clap.workspace = true
tokio.workspace = true
```

`crates/cg-cli/src/main.rs`：

```rust
//! cellglide: control the CellGlide service from a terminal.

#[cfg(unix)]
mod unix_main {
    use anyhow::{Result, bail};
    use cg_ipc::client::{self, ClientError};
    use cg_ipc::{Check, ConnState, Request, Response, describe};
    use clap::{Parser, Subcommand};
    use std::path::PathBuf;
    use std::time::Duration;

    #[derive(Parser)]
    #[command(name = "cellglide", about = "CellGlide：经 iPhone 的蜂窝网络上网")]
    struct Cli {
        /// Control socket of cellglide-service.
        #[arg(long, default_value = cg_ipc::SOCKET_PATH)]
        socket: PathBuf,
        #[command(subcommand)]
        command: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// 显示连接状态
        Status,
        /// 开始使用手机网络
        Connect {
            /// 插着多部 iPhone 时，指定其中一部的序列号
            #[arg(long)]
            serial: Option<String>,
        },
        /// 停止使用手机网络
        Disconnect,
        /// 检查环境并给出修复办法
        Doctor,
        /// 持续显示状态变化
        Watch,
    }

    fn print_snapshot(snapshot: &cg_ipc::Snapshot) {
        println!("{}", describe::state_line(snapshot));
        if let Some(line) = snapshot.metrics.as_ref().and_then(describe::metrics_line) {
            println!("{line}");
        }
    }

    fn service_problem(error: &ClientError) -> Check {
        let denied = matches!(error, ClientError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied);
        if denied {
            Check {
                name: "group".into(),
                ok: false,
                detail: "当前用户不在 cellglide 组，不能控制服务".into(),
                fix: Some("sudo usermod -aG cellglide $USER，然后注销并重新登录".into()),
            }
        } else {
            Check {
                name: "service".into(),
                ok: false,
                detail: format!("CellGlide 服务没有运行（{error}）"),
                fix: Some("sudo systemctl enable --now cellglide".into()),
            }
        }
    }

    async fn call(cli: &Cli, request: Request) -> Result<Response> {
        match client::request(&cli.socket, &request).await {
            Ok(Response::Error { message }) => bail!(message),
            Ok(response) => Ok(response),
            Err(error) => bail!("{}", describe::checks(&[service_problem(&error)]).trim_end()),
        }
    }

    #[tokio::main]
    pub async fn main() -> Result<()> {
        let cli = Cli::parse();
        match &cli.command {
            Cmd::Status => {
                if let Response::Status { snapshot } = call(&cli, Request::Status).await? {
                    print_snapshot(&snapshot);
                }
            }
            Cmd::Connect { serial } => {
                call(&cli, Request::Connect { serial: serial.clone() }).await?;
                let mut updates = client::subscribe(&cli.socket).await?;
                let waited = tokio::time::timeout(Duration::from_secs(20), async {
                    while let Some(snapshot) = updates.recv().await {
                        if matches!(snapshot.state, ConnState::Connected { .. } | ConnState::Retrying { .. } | ConnState::ChoosePhone | ConnState::NoPhone | ConnState::NoUsbService) {
                            return Some(snapshot);
                        }
                    }
                    None
                })
                .await;
                match waited {
                    Ok(Some(snapshot)) => print_snapshot(&snapshot),
                    _ => println!("仍在连接，可运行 cellglide watch 查看进度"),
                }
            }
            Cmd::Disconnect => {
                call(&cli, Request::Disconnect).await?;
                println!("已断开，电脑回到原来的网络");
            }
            Cmd::Doctor => {
                let checks = match client::request(&cli.socket, &Request::Doctor).await {
                    Ok(Response::Doctor { mut checks }) => {
                        checks.insert(0, Check { name: "service".into(), ok: true, detail: "CellGlide 服务正在运行".into(), fix: None });
                        checks
                    }
                    Ok(Response::Error { message }) => vec![Check { name: "service".into(), ok: false, detail: message, fix: Some("sudo usermod -aG cellglide $USER，然后注销并重新登录".into()) }],
                    Ok(_) => bail!("服务回复了意外的内容"),
                    Err(error) => vec![service_problem(&error)],
                };
                print!("{}", describe::checks(&checks));
                if checks.iter().any(|c| !c.ok) {
                    std::process::exit(1);
                }
            }
            Cmd::Watch => {
                let mut updates = client::subscribe(&cli.socket).await?;
                while let Some(snapshot) = updates.recv().await {
                    print_snapshot(&snapshot);
                }
                println!("服务已停止");
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix_main::main()
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("cellglide 第 0 阶段只支持 Linux")
}
```

- [ ] **Step 6: 手动验证**

Run: `cargo run -p cg-cli -- --socket /tmp/none.sock status; cargo run -p cg-cli -- --socket /tmp/none.sock doctor`
Expected: `status` 打印「✗ CellGlide 服务没有运行…」与修复命令并以非零退出；`doctor` 同样列出这一项并以退出码 1 结束；`cargo run -p cg-cli -- --help` 显示中文子命令说明。

- [ ] **Step 7: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-ipc crates/cg-cli && git commit -m "cellglide CLI and shared status text

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: iced 最小界面与托盘（`cg-gui`）

托盘用 ksni（StatusNotifierItem，走 D-Bus，不依赖 GTK）；iced 以 daemon 模式运行，关掉窗口后程序仍在托盘。Wayland 不能指定窗口位置，点托盘就打开一个普通小窗口。

iced 0.14 与 ksni 0.3 的接口以 docs.rs 上对应版本为准（`docs.rs/iced/0.14.0`、`docs.rs/ksni/0.3.6`）。下方代码按这两个版本写成；若编译报接口不符，只改调用方式，不改界面结构和消息。

**Files:**
- Create: `crates/cg-gui/Cargo.toml`, `crates/cg-gui/src/main.rs`, `crates/cg-gui/src/app.rs`, `crates/cg-gui/src/tray.rs`

**Interfaces:**
- Consumes: `cg_ipc::{client, describe, Request, Response, Snapshot, ConnState}`
- Produces: 可执行文件 `cellglide-gui`；`app::Message`（下方枚举）

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "cg-gui"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[[bin]]
name = "cellglide-gui"
path = "src/main.rs"

[dependencies]
cg-ipc.workspace = true
futures.workspace = true
iced.workspace = true
tokio.workspace = true

[target.'cfg(target_os = "linux")'.dependencies]
ksni.workspace = true
```

- [ ] **Step 2: 写失败的测试（状态到界面文字）**

`crates/cg-gui/src/app.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::ConnState;

    #[test]
    fn button_follows_wish_not_state() {
        let mut snapshot = Snapshot { state: ConnState::Retrying { reason: "x".into() }, wants_connection: true, ..Snapshot::default() };
        assert_eq!(button_label(Some(&snapshot)), "断开");
        snapshot.wants_connection = false;
        assert_eq!(button_label(Some(&snapshot)), "连接");
        assert_eq!(button_label(None), "连接");
    }

    #[test]
    fn title_shows_connection() {
        let connected = Snapshot { state: ConnState::Connected { phone_version: "2.2.0".into(), phone_dns: true }, ..Snapshot::default() };
        assert!(is_connected(Some(&connected)));
        assert!(!is_connected(Some(&Snapshot::default())));
        assert!(!is_connected(None));
    }
}
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p cg-gui`
Expected: 编译失败。

- [ ] **Step 4: 实现 `app.rs`**

```rust
//! One small window with the connection state, readings and the connect switch.
//! The app keeps running in the tray when the window is closed.

use cg_ipc::{ConnState, Request, Response, Snapshot, client, describe};
use iced::widget::{button, column, text};
use iced::{Element, Size, Subscription, Task, window};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Message {
    Snapshot(Snapshot),
    ServiceGone(String),
    OpenWindow,
    WindowClosed(window::Id),
    Toggle,
    CommandDone(Result<(), String>),
    Quit,
}

#[derive(Default)]
pub struct App {
    snapshot: Option<Snapshot>,
    problem: Option<String>,
    window: Option<window::Id>,
}

pub fn button_label(snapshot: Option<&Snapshot>) -> &'static str {
    if snapshot.is_some_and(|s| s.wants_connection) { "断开" } else { "连接" }
}

pub fn is_connected(snapshot: Option<&Snapshot>) -> bool {
    snapshot.is_some_and(|s| matches!(s.state, ConnState::Connected { .. }))
}

fn open_window() -> (window::Id, Task<Message>) {
    let (id, task) = window::open(window::Settings { size: Size::new(380.0, 240.0), resizable: false, ..window::Settings::default() });
    (id, task.discard())
}

impl App {
    pub fn boot() -> (App, Task<Message>) {
        let (id, task) = open_window();
        (App { window: Some(id), ..App::default() }, task)
    }

    pub fn title(&self, _window: window::Id) -> String {
        if is_connected(self.snapshot.as_ref()) { "CellGlide · 已连接".into() } else { "CellGlide".into() }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Snapshot(snapshot) => {
                self.snapshot = Some(snapshot);
                self.problem = None;
            }
            Message::ServiceGone(problem) => {
                self.snapshot = None;
                self.problem = Some(problem);
            }
            Message::OpenWindow => {
                if let Some(id) = self.window {
                    return window::gain_focus(id);
                }
                let (id, task) = open_window();
                self.window = Some(id);
                return task;
            }
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                }
            }
            Message::Toggle => {
                let request = self.snapshot.as_ref().map_or(Request::Connect { serial: None }, describe::toggle_request);
                return Task::perform(
                    async move {
                        match client::request(Path::new(cg_ipc::SOCKET_PATH), &request).await {
                            Ok(Response::Error { message }) => Err(message),
                            Ok(_) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        }
                    },
                    Message::CommandDone,
                );
            }
            Message::CommandDone(result) => self.problem = result.err(),
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }

    pub fn view(&self, _window: window::Id) -> Element<'_, Message> {
        let state = match (&self.snapshot, &self.problem) {
            (Some(snapshot), _) => describe::state_line(snapshot),
            (None, Some(problem)) => problem.clone(),
            (None, None) => "正在连接 CellGlide 服务…".into(),
        };
        let readings = self.snapshot.as_ref().and_then(|s| s.metrics.as_ref()).and_then(describe::metrics_line).unwrap_or_default();
        let mut content = column![text("CellGlide").size(22), text(state).size(15), text(readings).size(13)].spacing(12).padding(20);
        if let Some(problem) = self.problem.as_ref().filter(|_| self.snapshot.is_some()) {
            content = content.push(text(problem.clone()).size(13));
        }
        content.push(button(text(button_label(self.snapshot.as_ref()))).on_press(Message::Toggle)).into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([Subscription::run(crate::tray::events), window::close_events().map(Message::WindowClosed)])
    }
}
```

- [ ] **Step 5: 实现 `tray.rs`（托盘 + 服务订阅，同一条事件流）**

```rust
//! The tray icon and the service's snapshots, as one stream of app messages.

use crate::app::Message;
use cg_ipc::{client, describe};
use futures::channel::mpsc::Sender;
use futures::{SinkExt, Stream};
use std::path::Path;
use std::time::Duration;

#[cfg(target_os = "linux")]
struct Tray {
    tx: Sender<Message>,
    connected: bool,
    wants: bool,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "cellglide".into()
    }
    fn title(&self) -> String {
        "CellGlide".into()
    }
    fn icon_name(&self) -> String {
        if self.connected { "network-cellular-connected-symbolic" } else { "network-cellular-offline-symbolic" }.into()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(Message::OpenWindow);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "打开 CellGlide".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(Message::OpenWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.wants { "断开" } else { "连接" }.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(Message::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "退出".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(Message::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn events() -> impl Stream<Item = Message> {
    iced::stream::channel(32, |mut output: Sender<Message>| async move {
        #[cfg(target_os = "linux")]
        let tray = {
            use ksni::TrayMethods;
            // No tray host (GNOME without the AppIndicator extension): the window alone still works.
            Tray { tx: output.clone(), connected: false, wants: false }.spawn().await.ok()
        };
        loop {
            match client::subscribe(Path::new(cg_ipc::SOCKET_PATH)).await {
                Ok(mut updates) => {
                    while let Some(snapshot) = updates.recv().await {
                        #[cfg(target_os = "linux")]
                        if let Some(handle) = &tray {
                            let (connected, wants) = (crate::app::is_connected(Some(&snapshot)), snapshot.wants_connection);
                            handle.update(move |t: &mut Tray| {
                                t.connected = connected;
                                t.wants = wants;
                            })
                            .await;
                        }
                        if output.send(Message::Snapshot(snapshot)).await.is_err() {
                            return;
                        }
                    }
                    let _ = output.send(Message::ServiceGone("CellGlide 服务已停止".into())).await;
                }
                Err(error) => {
                    let text = describe::checks(&[cg_ipc::Check {
                        name: "service".into(),
                        ok: false,
                        detail: error.to_string(),
                        fix: Some("sudo systemctl enable --now cellglide".into()),
                    }]);
                    let _ = output.send(Message::ServiceGone(text.trim_end().into())).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}
```

- [ ] **Step 6: 实现 `main.rs`**

`cg_ipc::client` 只在 Unix 上编译，所以第 0 阶段界面整体限定在 Unix，Windows 上只编一个提示退出的 `main`：

```rust
#[cfg(unix)]
mod app;
#[cfg(unix)]
mod tray;

#[cfg(unix)]
fn main() -> iced::Result {
    iced::daemon(app::App::boot, app::App::update, app::App::view)
        .title(app::App::title)
        .subscription(app::App::subscription)
        .run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("cellglide-gui 第 0 阶段只支持 Linux");
}
```

- [ ] **Step 7: 编译与单元测试**

Run: `cargo test -p cg-gui && scripts/linux-test.sh -p cg-gui`
Expected: 2 个测试 `ok`，Linux 容器里编译通过（含 ksni）。

- [ ] **Step 8: 在 Linux 桌面上目测（Parallels ARM Ubuntu 24.04）**

在虚拟机里：

```bash
sudo apt install -y build-essential git iproute2 usbmuxd gnome-shell-extension-appindicator
git clone git@github.com:kikohz/CellGlide-Desktop.git && cd CellGlide-Desktop && git submodule update --init --recursive
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
cargo run -p cg-gui
```

Expected（服务未运行时）：出现「CellGlide」小窗口，中文正常显示，文字提示服务没有运行并给出 `systemctl` 命令；顶栏出现托盘图标；关掉窗口后程序不退出，点托盘图标或菜单「打开 CellGlide」重新出现窗口；菜单「退出」结束程序。截图保存到 `docs/validation/images/gui-no-service.png`。

- [ ] **Step 9: fmt、clippy、提交**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/cg-gui docs/validation/images && git commit -m "cellglide-gui: iced window and ksni tray

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17: 开发安装、实机验证与结论文档

**Files:**
- Create: `packaging/cellglide.service`, `scripts/install-dev.sh`
- Modify: `docs/validation/linux-phase0.md`（补全）、`README.md`（加开发说明）

**Interfaces:**
- Consumes: `cellglide-service`、`cellglide`、`cellglide-gui`、`scripts/network-snapshot.sh`

- [ ] **Step 1: systemd 单元**

`packaging/cellglide.service`：

```ini
[Unit]
Description=CellGlide: use the iPhone's cellular network over USB
After=network.target

[Service]
ExecStart=/usr/local/bin/cellglide-service
Restart=on-failure
RestartSec=2
RuntimeDirectory=cellglide
RuntimeDirectoryMode=0755
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
```

（不加 `Wants=usbmuxd.service`：usbmuxd 由 udev 在插上 iPhone 时启动，服务自己每 2 秒重试连接。）

- [ ] **Step 2: 开发安装脚本**

`scripts/install-dev.sh`：

```sh
#!/bin/sh
# Development install on this Linux machine: release binaries, the systemd service and
# the cellglide group. Checks other links' DNS and routes are unchanged (AGENTS.md).
# Undo with: scripts/install-dev.sh --uninstall
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
BIN=/usr/local/bin

if [ "${1:-}" = "--uninstall" ]; then
    sudo systemctl disable --now cellglide 2>/dev/null || true
    sudo rm -f "$BIN/cellglide-service" "$BIN/cellglide" "$BIN/cellglide-gui" /etc/systemd/system/cellglide.service
    sudo systemctl daemon-reload
    echo "已卸载。cellglide 组保留，如需删除：sudo groupdel cellglide"
    exit 0
fi

command -v usbmuxd >/dev/null 2>&1 || echo "提示：还没有安装 usbmuxd。Ubuntu / Debian：sudo apt install usbmuxd；Fedora：sudo dnf install usbmuxd"
cargo build --release -p cg-service -p cg-cli -p cg-gui
mkdir -p target
scripts/network-snapshot.sh > target/network-before.txt

# Stop the old engine before replacing its binary; its interface must be gone.
if systemctl is-active --quiet cellglide; then
    sudo systemctl stop cellglide
fi
if [ -e /sys/class/net/cellglide0 ]; then
    echo "cellglide0 仍然存在，旧服务没有停干净，已中止安装"
    exit 1
fi

sudo install -m 0755 target/release/cellglide-service target/release/cellglide target/release/cellglide-gui "$BIN/"
sudo install -m 0644 packaging/cellglide.service /etc/systemd/system/cellglide.service
getent group cellglide >/dev/null || sudo groupadd --system cellglide
if ! id -nG "$USER" | grep -qw cellglide; then
    sudo usermod -aG cellglide "$USER"
    echo "已把 $USER 加入 cellglide 组，注销并重新登录后生效（在此之前可用 sudo cellglide …）"
fi
sudo systemctl daemon-reload
sudo systemctl enable --now cellglide

scripts/network-snapshot.sh > target/network-after.txt
if diff -u target/network-before.txt target/network-after.txt; then
    echo "安装完成；其他网卡的 DNS 与路由没有变化"
else
    echo "警告：安装前后其他网卡的网络设置不同，见上方差异"
    exit 1
fi
```

Run: `chmod +x scripts/install-dev.sh && sh -n scripts/install-dev.sh`
Expected: 语法检查无输出。

- [ ] **Step 3: README 开发说明**

在 `README.md` 末尾追加：

````markdown
## 开发

- 在 macOS 上开发：`cargo test --workspace`；Linux 专有部分：`scripts/linux-test.sh`（需要 Docker / colima）。
- 需要 root 与 systemd-resolved 的测试只在 CI（`privileged` 任务）里跑。
- 在 Linux 机器上安装开发版：

```bash
scripts/install-dev.sh
```

- 常用命令：`cellglide status`、`cellglide connect`、`cellglide disconnect`、`cellglide doctor`、`cellglide watch`、`cellglide-gui`。
- 卸载：`scripts/install-dev.sh --uninstall`。
````

- [ ] **Step 4: 结论文档模板**

用下面内容替换 `docs/validation/linux-phase0.md`（保留 Task 2 记下的 RESULT 行，填进第 1 节）：

```markdown
# Linux 第 0 阶段结论

日期：（填写）
测试机：（型号、CPU、发行版与内核版本）
iPhone：（型号、iOS 版本、CellGlide 版本）

## 1. resolved 回环端口验证

- CI ubuntu-24.04：（Task 2 的 RESULT 行）
- 实机：`resolvectl dns cellglide0` 输出：（粘贴）

## 2. 实机清单

| # | 项目 | 做法 | 结果 |
|---|---|---|---|
| 1 | 出口 IP 为蜂窝 IP | `cellglide connect` 后 `curl -s https://www.cloudflare.com/cdn-cgi/trace` 查看 `ip=`，与手机关掉 Wi‑Fi 时的出口对比 | |
| 2 | DNS 不泄漏 | 浏览器打开 dnsleaktest.com 标准测试；结果只应出现手机 DoH 服务商 | |
| 3 | UDP 可用 | `dig @1.1.1.1 example.com`；浏览器打开 cloudflare-quic.com 显示 HTTP/3 | |
| 4 | 拔线后网络自动恢复 | 连接中拔线，`ip link show cellglide0` 不存在，`curl` 经原网络成功，耗时（秒） | |
| 5 | `kill -9` 不留痕迹 | `sudo pkill -9 cellglide-service` 后：无 `cellglide0`、`ip route` 无 `0.0.0.0/1`、`resolvectl dns` 无 1053 | |
| 6 | 其他网卡 DNS 前后一致 | 每项测试前后运行 `scripts/network-snapshot.sh` 并 `diff` | |
| 7 | 插回自动重连 | 拔线后重新插上，`cellglide watch` 显示重新连接 | |
| 8 | 手机关掉 CellGlide DNS | 状态显示「使用公共 DNS」，`resolvectl dns cellglide0` 为四个公共地址 | |
| 9 | 界面 | 托盘、窗口、开关、中文、关窗后仍在托盘（附截图） | |

## 3. 性能

| 项目 | 结果 |
|---|---|
| 下载（`curl -o /dev/null` 大文件，或 speed.cloudflare.com） | |
| 上传 | |
| 延迟（`ping 1.1.1.1` 中位数） | |
| 服务 CPU（下载满速时 `top`） | |
| 服务内存（`ps -o rss`） | |
| 界面内存 | |

同一手机、同一地点，与 Mac 版对比：（填写）

## 4. 问题清单

（每条：现象、复现步骤、原因、是否阻碍进入首版）

## 5. 建议

（是否进入 Linux 首版；首版需要先解决的问题；对 Windows 的影响）
```

- [ ] **Step 5: 提交**

```bash
git add packaging scripts/install-dev.sh docs/validation/linux-phase0.md README.md
git commit -m "Dev install script, systemd unit and phase 0 validation template

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: x86 实机测试（需要用户配合插 iPhone）**

在 x86 Ubuntu 24.04 实机上：

```bash
sudo apt install -y build-essential git iproute2 usbmuxd
git clone git@github.com:kikohz/CellGlide-Desktop.git && cd CellGlide-Desktop && git submodule update --init --recursive
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
scripts/install-dev.sh
cellglide doctor
```

插上 iPhone、解锁并信任，手机上打开 CellGlide 开始共享，然后逐项完成第 2、3 节的表格。每完成一项就把结果填进 `docs/validation/linux-phase0.md`。

- [ ] **Step 7: 填写结论并提交**

全部清单通过且 CI 全绿即达到退出条件；未通过的项目写进第 4 节并给出建议。

```bash
git add docs/validation && git commit -m "Linux phase 0 validation results

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```
