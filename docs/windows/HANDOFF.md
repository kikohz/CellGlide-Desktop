# Windows 1.0 开发交接

日期：2026-10-29。写给在 Windows 测试机上开新会话、接手 Windows 版开发的人。

## 当前状态

- **仓库**：公开仓库 `kikohz/CellGlide-Desktop`，MIT 许可，2026-10-28 开源。Windows 开发在 `windows` 分支上做，它从 main 分出来，目前和 main 一致。
- **完整开发历史**：保存在私有存档 `kikohz/CellGlide-Desktop-dev` 里，包括 Linux 各里程碑的提交和 `v1.0.0` 标签。公开仓库只有一个初始提交。
- **Linux 1.0.0**：已于 2026-10-21 发布到 pkg.cellglide.com（apt 和 rpm）。Windows 版的功能以它为准。
- **Windows 部分进度**：调研已经做完，设计文档还没写，代码一行都还没动。

## 用户已经定下的事

1. **功能范围**：对标 Linux 1.0，即托盘面板、手机卡片、测速、DNS 泄漏检测、连接记录、诊断、设置、关于。spec 见 `docs/specs/2026-10-16-linux-1.0-full-design.md`。
2. **架构**：同时支持 **x64 和 ARM64**。
3. **连 iPhone 依赖 Apple 官方组件**，也就是微软商店里的「Apple 设备」应用。它提供 Apple Mobile Device Service，在 `127.0.0.1:27015` 上跑 usbmux 协议，和 Linux 的 usbmuxd 是同一套协议。我们自己**不带任何驱动**，也不自带 usbmuxd：换驱动会弄坏 iTunes 和「Apple 设备」，还需要驱动签名。安装脚本检测到服务不存在时，提示用户安装，或者在用户同意后用 `winget --source msstore` 帮他装。
4. **安装方式**：PowerShell 一行命令，形如 `irm https://pkg.cellglide.com/install.ps1 | iex`。不做 MSI 安装器，同时提供 zip 直链。卸载也给一行命令。
5. **签名**：第一版不签名。之后申请 SignPath Foundation 的免费签名，条件是开源、在 CI 公开构建、已经发布过一版。
6. **界面用 WinUI 3（C#）**，核心用 Rust。结构是：
   - **Rust 系统服务**：连 iPhone、虚拟网卡、路由、DNS、健康判定、连接记录、诊断。尽量和 Linux 共用 cg-core 等 crate。
   - **Rust 动态库 `cellglide.dll`**：通过 C 接口提供三样东西：和服务通信（订阅状态、发命令）、测速和 DNS 泄漏检测引擎（复用 cg-tools）、翻译后的文案（文案只有 cg-i18n 的 yml 这一份）。C# 端用 csbindgen 之类的工具生成绑定。
   - **C# WinUI 3 界面**：只负责显示和交互。托盘用 H.NotifyIcon。不打包成 MSIX，采用自包含部署。

   这一条是用户提出的方向，具体怎么划分边界，由设计文档最终确定。
7. **开发全部在 Windows 测试机上进行**，不在用户的 Mac 上。

## 调研结论

详见同目录下的两份报告。

- **`research-apple.md`**（Apple 驱动和服务）
  - Apple 的 USB 驱动 usbaapl 有原生 ARM64 版，由 Windows Update 推送。「Apple 设备」应用在 ARM64 上要求 Windows 11 24H2 或更新。
  - Apple 的 USB 网卡驱动 netaapl 没有 ARM64 版，但我们用不到它。
  - 配对记录在 `C:\ProgramData\Apple\Lockdown`。ReadPairRecord 能用。
  - Windows 上「信任这台电脑」的弹窗由谁触发，还要实机确认。如果服务不会自己弹，就参考 go-ios 自己实现配对。
- **`research-net.md`**（网络接管）
  - **hev-socks5-tunnel 目前只能在 MSYS2 下编译**，而且只有 x64，所以 Windows 第一版建议用 Rust 转发库（tun2proxy 或 ipstack），放在 `cg_core::tunnel::TunnelBackend` 后面。这样会和 Mac、Linux 的引擎不一致，要在设计里写明。
  - **虚拟网卡**：用 wintun，它有 ARM64 签名的 dll，可以按原样随包分发。
  - **路由**：只在自己的网卡上加四条 /1 路由，跃点设 0。手机走本机 27015 端口，不需要绕过路由。只有测速和上行对比要用 `IP_UNICAST_IF` 绑到物理网卡。
  - **DNS**：只在自己的网卡上设置，解析器监听 `198.18.0.1:53`，因为 Windows 的接口 DNS 不能指定端口。出错时要立刻回 SERVFAIL。**默认不用 NRPT**，它是系统级持久配置，崩溃后会残留。可以加一个「严格 DNS」开关，用 WFP 动态会话实现。
  - **服务**：以 LocalSystem 运行。和界面之间用命名管道通信，按客户端令牌授权。睡眠唤醒用 `SERVICE_CONTROL_POWEREVENT`。
  - **安装后的提示**：用 `Invoke-WebRequest` 下载的文件不带网络标记。Windows 11 的「智能应用控制」开着时，会拦截未签名程序。
- **现有代码怎么移植**
  - cg-protocol、cg-i18n 可以直接用。
  - cg-core 基本可移植：`MuxEndpoint::Tcp` 已经有了。另外几项要处理：测试用的 FakeMux 要加 TCP 版，`usb_speed` 读的是 sysfs，健康日志的文件权限也要改。
  - 要新写 Windows 模块的部分，按工作量估算：
    - 大：cg-platform（wintun、路由、DNS）、cg-service（SCM、命名管道、电源事件、诊断来源）；
    - 中：cg-ipc（传输层抽象）、cg-cli、cg-tools（上行检测、`IP_UNICAST_IF`、路径）；
    - 小：cg-diag（平台字符串、UA、路径脱敏规则）。另外，诊断服务端目前对 Windows 的 UA 返回 403，需要改一下服务端，代码在私有的 CellProxy 仓库。
  - 现有界面 cg-gui 是 iced 写的。如果 Windows 用 WinUI 3，它在 Windows 上就不编译；里面的页面逻辑可以拿来参考。

## 测试机

- **系统**：Windows 11 专业版 23H2（22631），x64，i7-1195G7，16 GB 内存。
- **账号**：用户 `iloss` 是管理员，提权不弹 UAC。
- **安全设置**：「智能应用控制」是关闭的，Defender 开着。
- **已装的工具**：Git、Rustup、.NET SDK 10、VS 2022 Build Tools（C++、ARM64 工具链、Windows 11 SDK）。都是 2026-10-29 通过 winget 装的。
- **需要用户自己装**：「Apple 设备」，要在微软商店里登录并点同意。
- **ARM64 实机**：目前没有。可以在 Apple 芯片的 Mac 上用 Windows 11 ARM 虚拟机测，也可以用 GitHub 的 `windows-11-arm` 运行器，公开仓库免费。

## 工作方式

沿用 Linux 版的流程：先写设计文档（`docs/specs/`），用户确认后写实施计划（`docs/superpowers/plans/`），再逐任务交给子代理实现并审查，最后做实机清单。用户的习惯和约束：

- 始终用简体中文回复。
- 技术选型由开发者拍板；产品、范围、运维上的决定交给用户。
- 对外动作每一步都要先问用户：发布、推送到 main、部署、删除云端资源。
- `AGENTS.md` 的原则在 Windows 上照样适用：只改 CellGlide 自己的网卡和 DNS，不碰 Wi‑Fi、以太网、其他 VPN 等别的网卡的设置；安装和测试前后都要对比网络配置。
- 构建产物不要放进仓库；脚本优先用 PowerShell 或 Python。
- 截图和宣传材料只用英文界面。
