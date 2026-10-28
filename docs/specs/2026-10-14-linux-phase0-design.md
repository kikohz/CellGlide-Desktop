# Linux 第 0 阶段技术验证设计

日期：2026-10-14
状态：设计已确认，待写实施计划。
前置文档：CellProxy 仓库的 `Docs/Specs/2026-10-10-linux-windows-evaluation.md`（本文对其中两处结论做了更新，见第 1 节）。

仓库约定：本仓库（CellGlide-Desktop）放 Linux 与 Windows 客户端；iPhone App 与 Mac 客户端在 CellProxy 仓库。下文以 `CellProxy:` 开头的路径指 CellProxy 仓库。手机协议以 CellProxy 仓库为准，本仓库的协议测试数据注明对应的协议版本。

## 1. 目标与已定事项

在 Linux 上用 Rust 跑通「插 iPhone → 经手机蜂窝网络上网」的最小闭环，验证技术路线，再决定是否进入 Linux 首版。

已定事项：

- **Linux 与 Windows 共用一套 Rust 代码**，Mac 保持 Swift 原生不动。平台相关部分放在 trait 后面；第 0 阶段只实现 Linux，Windows 为空实现，只要求 CI 能编译。
- **流量引擎沿用 hev-socks5-tunnel**，与 Mac 相同。编成静态库链接进服务。不用 tun2proxy：三平台同一个引擎，问题能对照排查。
- **界面框架定为 iced**（评估文档原写「第 0 阶段对比 iced 与 egui 后再定」，现直接定为 iced，不再做对比）。理由：
  - 自带 daemon 模式，没有窗口也能常驻托盘；
  - 中文自动回退系统字体；
  - 没有 GPU 时自动改用软件渲染；
  - Elm 架构贴合连接状态机。

  短板是无障碍不完整、文档少；对策是界面层只做显示，换框架成本低。托盘用 `tray-icon` 的 ksni 后端（StatusNotifierItem over D-Bus，不依赖 GTK）。
- **iPhone 端不改**，协议与 Mac 完全一致。

## 2. 范围

做：

- usbmuxd 发现设备与建立通道；
- 与手机 App 握手、会话、状态、网速、电量；
- 本地 SOCKS 转发与 UDP 桥（0xF0）；
- TUN + hev + 路由；
- 本地 DNS 解析器 + systemd-resolved 单网卡 DNS；
- root 服务与控制通道；
- 命令行；
- iced 最小界面；
- CI；
- 开发用安装脚本；
- 结论文档。

不做：

- 私网绕过、分流、规则集、测速、目的地地图、DNS 泄漏页、故障时间线；
- 插线自动连接；
- 安装包（deb / rpm / AppImage）、自动更新；
- Windows 实际功能。

## 3. 结构

本仓库根目录是一个 Cargo workspace，crate 放在 `crates/` 下。

| crate | 职责 | 平台相关性 |
|---|---|---|
| `cg-protocol` | usbmuxd plist 协议；手机 9876 / 8088 / 8089 / 8090 各端口协议；UDP-over-USB 帧（`CellProxy:Docs/USB-UDP-v1.md`） | 纯逻辑，无 IO |
| `cg-core` | 设备发现、本地转发、UDP 桥、本地 DNS 解析器、健康状态机；基于 tokio | 只依赖 `cg-platform` 的 trait |
| `cg-platform` | 实现 `cg-core` 定义的 `TunnelBackend` trait（建网卡、路由、DNS、hev 一次完成）；`linux/` 为实际实现，Windows 暂不实现 | 平台相关 |
| `hev-sys` | build.rs 编译固定版本的 hev（与 Mac 同为 2.18.x）为静态库并导出 FFI；Windows 上暂不编译 | C |
| `cg-ipc` | 控制通道消息类型（JSON） | 纯逻辑 |
| `cg-service` | 可执行文件 `cellglide-service`，root 运行 | — |
| `cg-cli` | 可执行文件 `cellglide` | — |
| `cg-gui` | 可执行文件 `cellglide-gui`，iced + tray-icon | — |

进程划分与 Mac 不同：Mac 的转发和 DNS 在用户态 App 里，Helper 只管 TUN。Linux 把核心全部放进 root 服务，界面和命令行只是客户端。这样有三个好处：没有界面或托盘时也能用；没有桌面的机器也能用；Windows 服务可以照搬这套结构。

```
cellglide-service（root，systemd）
  ├─ usbmuxd：Listen 感知插拔，Connect 连手机端口
  ├─ 本地 SOCKS 127.0.0.1:<随机> → 手机 9876；UDP 桥（0xF0）
  ├─ 握手、watch 会话、状态、网速与手机电量读数（8088–8090）
  ├─ 本地 DNS 解析器 127.0.0.1:1053 → 手机 DoH
  ├─ TUN cellglide0 + hev + 半路由 + resolved 单网卡 DNS
  └─ 控制 socket /run/cellglide/control.sock
cellglide（命令行）、cellglide-gui（iced）：普通用户，经控制 socket 与服务通信
```

## 4. 连接与流量

**连手机**：Linux 开源 usbmuxd 的 socket 是 `/var/run/usbmuxd`。消息头为 16 字节小端，类型 8 为 plist；用 `ListDevices`、`Listen`、`Connect`（端口大端）、`ReadPairRecord`。照 `CellProxy:Sources/DesktopCore/MuxSocket.swift` 重写。

**接管流量**：数值照搬 `CellProxy:Sources/DesktopCore/TunnelConfiguration.swift`，三平台一致。

- 网卡 `cellglide0`（`/dev/net/tun`，TUNSETIFF），地址 198.18.0.1 和 fd6c:6779:6465::1，MTU 1500。
- hev 配置与 Mac 的 `relay(socksPort:)` 相同：TCP 超时 1800 s，UDP 30 s。
- 用 `ip` 命令（iproute2）配地址并加四条半路由：`0.0.0.0/1`、`128.0.0.0/1`、`::/1`、`8000::/1`，出口为 `cellglide0`。局域网的直连路由更具体，不受影响。
- 不会形成回环：hev 只连 127.0.0.1；转发出口是 usbmuxd 的 Unix socket，不经过 TUN。

**DNS**：

- 解析器上游与 Mac 相同：手机 DoH，即 `http://127.0.0.1:<转发端口>/dns-query`；手机关闭 DNS 时，改走经隧道的公共 DNS（`fallbackDNSServers`）。
- 经 systemd-resolved 的 D-Bus 接口，只对 `cellglide0` 调用：
  - `SetLinkDNSEx`（127.0.0.1，端口 1053）；
  - `SetLinkDomains`（`~.`）；
  - `SetLinkDefaultRoute(true)`。
- 返回真实 DNS 地址，不用假 IP。
- **不改 `/etc/resolv.conf`，不改其他网卡，不改 NetworkManager 保存的连接**（AGENTS.md）。
- 系统没有用 resolved 时，不接管 DNS，由 `doctor` 说明原因。
- **首个待验证项**：resolved 是否接受回环地址加非 53 端口作为单网卡 DNS。不接受则改为解析器监听 198.18.0.1:53。

**清理**：

- 正常停止：先 `RevertLink`，再关 TUN fd。
- 服务崩溃：内核随 fd 关闭删除网卡和路由，resolved 随网卡消失丢弃该网卡的配置。效果等同 Mac 会话键的「进程退出即失效」，不需要恢复逻辑。

**与 Mac 的有意差异（第 0 阶段）**：
- 路由与地址用 `ip` 命令而非 netlink 库：Mac 也直接调用 `route` / `ifconfig`，命令行便于排查，rtnetlink 0.23 接口变动大。
- 不做 8091（把 lockdown 读到的电池信息推给手机）：需要 lockdown TLS，留到首版；手机电量读数照常从 8090 获取。
- 出口检测用经手机的 SOCKS CONNECT（目标 www.cloudflare.com:443，8 秒超时）代替 Mac 的 curl HTTPS 请求，不引入 TLS 依赖。
- usbmuxd 连接方式（Unix socket 或 Windows 的 TCP 27015）放在 `cg-core` 的 `MuxEndpoint` 枚举里，不单独做 trait。

## 5. 控制通道与服务

- socket `/run/cellglide/control.sock`，权限 0660，组 `cellglide`；每个连接用 `SO_PEERCRED` 校验，只接受 root 或组成员。
- 协议为逐行 JSON。请求：`status`、`connect`、`disconnect`、`doctor`、`subscribe`。`subscribe` 之后服务主动推送状态变化。Windows 以后用命名管道，消息格式不变。
- systemd 单元 `cellglide.service`：
  - `Restart=on-failure`；
  - 加固选项 `ProtectSystem=strict`、`ProtectHome=yes`、`PrivateTmp=yes`、`RuntimeDirectory=cellglide`。
- 第 0 阶段以 root 运行；改为「专用用户 + CAP_NET_ADMIN + polkit 规则」留到首版。
- 第 0 阶段手动 `connect`。服务始终运行并监听设备，首版加自动连接不改结构。

## 6. 异常处理

与 Mac 健康逻辑一致。

| 情况 | 处理 |
|---|---|
| 拔线、App 退出、watch 会话断开 | 立即拆 TUN，回到原网络 |
| hev 退出 | 停止接管，`status` 报原因 |
| resolved 调用失败 | 停止接管，`status` 报原因 |
| usbmuxd 断开 | 停止接管，`status` 报原因 |
| 握手不兼容 | 不建 TUN，提示升级手机 App |

`doctor` 逐项检查以下内容，每项给出结论和修复命令：

- usbmuxd 是否安装并运行；
- 手机是否已信任这台电脑（有无配对记录）；
- 系统是否使用 systemd-resolved；
- `/dev/net/tun` 是否可用；
- 用户是否在 `cellglide` 组；
- 服务是否运行。

## 7. 界面（iced 最小样子）

- 托盘图标显示连接状态；菜单包含「打开窗口」「连接 / 断开」「退出」。
- 一个小窗口：手机名、连接状态、上下行网速、电量、开关。
- 中文显示用系统字体。
- daemon 模式：关掉窗口后程序仍在托盘。

Wayland 不允许指定窗口位置，点托盘打开的是普通窗口。GNOME 没有托盘扩展时，程序仍能以普通窗口使用。

## 8. 测试

- **单元测试**：参照 `CellProxy:Tests/DesktopCoreTests` 中协议、健康判断、UDP 帧（`SOCKSDatagramTests`）、本地解析器（`LocalResolverTests`）、手机指标、电量等用例，在 Rust 里写对照测试。
- **核心测试**：进程内假 usbmuxd 和假手机服务，覆盖连接、断线、恢复。
- **CI 集成测试**（GitHub Ubuntu 机器，sudo）：
  - 真实创建 TUN，配合假 SOCKS，检查路由与 resolved 单网卡 DNS；
  - 测试前后对比其他网卡的 DNS（`resolvectl` 输出）、`/etc/resolv.conf`、NetworkManager 连接文件哈希，必须一致。
- **Windows**：CI 里 `cargo check`。
- **实机清单**（x86 Ubuntu 24.04 插 iPhone；开发时可辅以 Parallels ARM Ubuntu）：
  1. 出口 IP 为蜂窝 IP；
  2. DNS 不泄漏；
  3. UDP 可用（dig、QUIC）；
  4. 拔线后网络自动恢复；
  5. `kill -9` 服务后不留网卡、路由、DNS 设置；
  6. 其他网卡 DNS 前后一致。

## 9. 交付物与退出条件

交付物：

- 本仓库代码；
- `.github/workflows/ci.yml`；
- `scripts/install-dev.sh`（安装二进制、systemd 单元、建组）；
- iced 最小界面；
- 结论文档 `docs/validation/linux-phase0.md`，内容包括 resolved 端口验证结果、速度与 CPU / 内存实测、问题清单、是否进入首版的建议。

退出条件：第 8 节实机清单全部通过，CI 全绿。

## 10. 风险

1. resolved 不接受回环加端口：备选方案见第 4 节。
2. 虚拟机 USB 直通 iPhone 不稳定：以 x86 实机结论为准。
3. hev 在 Windows 2.18.0 上有缺网卡名即崩溃的报告：Windows 阶段配置里显式写网卡名。
4. iced 0.15 前可能有不兼容改动：固定版本，界面层保持薄。
