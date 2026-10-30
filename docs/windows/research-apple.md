# CellGlide Windows 调研：Apple 组件、usbmux、ARM64

调研日期：2026-10-22。只读调研，没有改动任何代码。
范围：Windows 10/11 的 x64 和 ARM64。CellGlide 只走 usbmux，不用个人热点。

## 0. 我们实际用到的 usbmuxd 功能（来自代码）

- `crates/cg-protocol/src/mux.rs`：只走 plist 协议（16 字节头部，version 1，type 8）。用到四种消息：
  - `ListDevices`
  - `Listen`，接收 `Attached` / `Detached` 事件
  - `Connect`，参数是 `DeviceID` 和网络字节序的 `PortNumber`
  - `ReadPairRecord`，参数是 `PairRecordID`，返回 `PairRecordData`

  每条消息都会带上 `ClientVersionString`、`ProgName` 和 `kLibUSBMuxVersion=3`。
- `crates/cg-core/src/mux.rs`：Unix 用 `/var/run/usbmuxd`。非 Unix 平台已经写好 `TCP 127.0.0.1:27015`，连接失败时映射成 `NoUsbService`。
- `crates/cg-core/src/lockdown.rs`：从 pair record 里取出 `HostID`、`SystemBUID`、`HostCertificate`、私钥等。随后通过 usbmux `Connect` 连到 62078，发 `StartSession` 并升级为 TLS，再 `StartService com.apple.mobile.diagnostics_relay` 读电量。
- 客户端不会自己发 lockdown `Pair`，也不用 `SavePairRecord`、`ReadBUID`。配对完全靠宿主机上的 usbmuxd 已经存好的记录。
- ipheth 只在 `crates/cg-tools/src/uplink.rs` 里出现，用来把 USB 共享网卡排除在“上游网卡”之外。核心数据通路不依赖它。

## 1. Apple Mobile Device Service (AMDS)

### 1.1 位置和协议：已确认是 127.0.0.1:27015，协议相同

- libusbmuxd 在 `_WIN32` 下执行 `socket_connect("127.0.0.1", USBMUXD_SOCKET_PORT)`，Windows 不支持 `UNIX:` 前缀。读取完 `USBMUXD_SOCKET_ADDRESS` 后，plist 和二进制协议的选择逻辑与其他平台共用。
  https://github.com/libimobiledevice/libusbmuxd/blob/master/src/libusbmuxd.c
- go-ios 在 `GetUsbmuxdSocket()` 里，`case "windows": return "tcp://127.0.0.1:27015"`。
  https://github.com/danielpaulus/go-ios/blob/main/ios/usbmuxconnection.go
- pymobiledevice3 定义了 `ITUNES_HOST = ("127.0.0.1", 27015)  # used on Windows`。
  https://github.com/doronz88/pymobiledevice3/blob/master/pymobiledevice3/usbmux.py
- 2026-09 有一份实测（pymobiledevice3 #1968）：Store 版 Apple Devices 的进程路径是 `C:\Program Files\WindowsApps\AppleInc.AppleDevices_1.1540.24088.0_x64__nzyj5cx40ttqa\AppleMobileDeviceProcess.exe`，监听 `TCP 127.0.0.1:27015`。
  https://github.com/doronz88/pymobiledevice3/issues/1968
- 结论：现有的 `MuxEndpoint::Tcp(127.0.0.1:27015)` 和 plist 编解码可以直接复用。Windows 版 AMDS 就是 iTunes 用的那个 usbmuxd，go-ios 和 pymobiledevice3 都在 Windows 上用 plist 协议跑 ListDevices、Listen、Connect、ReadPairRecord。

### 1.2 哪些安装包会提供 AMDS

| 来源 | 形态 | 架构 | 备注 |
|---|---|---|---|
| Apple Devices（Microsoft Store 9NP83LWLPZ9K） | MSIX，服务进程是 WindowsApps 下的 `AppleMobileDeviceProcess.exe` | **x64 + arm64**。包 1.1540.24088.0 同时列出 `['x64','arm64']`，最低系统 10.0.26100.0（24H2）。旧包 1.1540.24087.0 只有 x64，最低 19045 | Apple 目前推荐的路径（support.apple.com/108643 只提这个 app）。据 pymobiledevice3 记录，这一版不做 Wi-Fi 发现，与我们无关 |
| iTunes（Store 9PB2MZ1ZMB1S） | AppxBundle | x86 + x64，没有 arm64 | 最低 16299 |
| iTunes（Apple 官网 Win64 安装包） | 内含 AppleMobileDeviceSupport64.msi | x64 | 经典路径 `C:\Program Files\Common Files\Apple\Mobile Device Support\` |
| 独立的 Apple Mobile Device Support MSI（winget `Apple.AppleMobileDeviceSupport` 19.4.0.10） | WiX MSI，machine scope | x86 / x64。x64 安装项的 `UnsupportedOSArchitectures` 只排除 arm 和 x86，**没有排除 arm64**，所以 ARM64 上 winget 会选 x64 MSI | 安装包直链在 swcdn.apple.com，见 manifest |

数据来源：
- Store catalog API：`https://displaycatalog.mp.microsoft.com/v7.0/products?bigIds=9NP83LWLPZ9K&market=US&languages=en-us`，以及 bigIds=9PB2MZ1ZMB1S
- winget manifest：https://github.com/microsoft/winget-pkgs/tree/master/manifests/a/Apple/AppleMobileDeviceSupport/19.4.0.10
- Apple 文档：https://support.apple.com/guide/devices-windows/install-the-apple-devices-app-mchl5ded2763/windows

### 1.3 第三方能否依赖 AMDS，怎么检测

- 可以依赖。iMazing、3uTools、go-ios、pymobiledevice3 都依赖 Apple 的组件。iMazing 2.17 起，它的安装程序会从 Microsoft Store 拉 Apple 组件，放到 `C:\ProgramData\DigiDNA\iMazing\MobileDevice`。
  https://imazing.com/guides/imazing-cli-documentation
  https://imazing.com/guides/fully-reinstall-apple-components-on-windows-pc
- 服务名：`Apple Mobile Device Service`。注册表在 `HKLM\SYSTEM\CurrentControlSet\Services\Apple Mobile Device Service`，用 `ImagePath` 区分两种安装：
  - 路径含 `windowsapps` 的是 Store 版 Apple Devices
  - 其余是 iTunes 或 MSI，路径在 `...\Common Files\Apple\Mobile Device Support\AppleMobileDeviceService.exe`

  pymobiledevice3 的 `osu/win_util.py` 就是这样判断的，而且普通用户不用提权就能读这个键。
  https://github.com/doronz88/pymobiledevice3/blob/master/pymobiledevice3/osu/win_util.py
- 建议的检测顺序：
  1. 读上面的服务注册表键：键不存在说明没装。
  2. 用 `OpenSCManager` / `QueryServiceStatus`（或 `sc query "Apple Mobile Device Service"`）看服务是否在运行。
  3. 最终以能否连上 TCP 127.0.0.1:27015 并拿到 `ListDevices` 的回复为准。代码里已有 `NoUsbService` 映射。
- 驱动是否就绪：先看设备管理器里有没有 `Apple Mobile Device USB Composite Device` / `Apple Mobile Device USB Device`（usbaapl64）。如果 iPhone 显示黄色感叹号或错误 Code 1、10，说明驱动没装好，常见于 ARM64，见第 3 节。可以用 SetupAPI 或 `pnputil /enum-devices /connected` 检查 VID 05AC 的设备。

## 2. 配对记录 (pair record)

- 位置：`%ALLUSERSPROFILE%\Apple\Lockdown`，即 `C:\ProgramData\Apple\Lockdown\<UDID>.plist`，另有 `SystemConfiguration.plist`（存 SystemBUID）。pymobiledevice3 的 `pair_record_path` 就是这个路径。
- 权限：
  - Lockdown 目录和其中的文件由 SYSTEM 拥有。2019 年的研究报告（decoder.cloud / Security Affairs）称普通用户可以在目录里新建文件，但 `<UDID>.plist` 只有读权限，而且服务在重新插线时会对文件执行 SetSecurity。这一点曾被用来提权，Apple 当时没有修。
    https://securityaffairs.com/posts/iphone-printconfig-dll-exploitation
  - 当前版本的 ACL 没有找到一手资料，只能当作未验证：**不要依赖直接读文件**，Store 版和以后的版本都可能收紧。
- ReadPairRecord 在 Windows 上可用：
  - go-ios 在各平台一律通过 usbmux 的 `ReadPairRecord` 取记录，没有 Windows 专用的回退（ios/pair_read.go）。
  - pymobiledevice3 的 `get_preferred_pair_record()` 依次尝试 usbmuxd、iTunes 目录（即 ProgramData\Apple\Lockdown）、本地缓存。这说明 usbmuxd 是首选，读文件只是兜底。
  - 结论：现在的做法（只用 ReadPairRecord）可以原样用在 Windows 上。可选加一层兜底：读失败时再试 `ProgramData\Apple\Lockdown\<UDID>.plist`。
- 风险：没有 pair record 时（从未点过“信任”），我们完全拿不到 lockdown 能力，见第 6 节。

## 3. ARM64（Snapdragon X）——关键结论

### 3.1 一手证据（Microsoft Update Catalog）

usbaapl 是 Apple 的 USB 驱动，WHQL 认证，通过 Windows Update 分发。**它有 ARM64 原生版本**：

| 驱动 | 版本 / 日期 | 架构 |
|---|---|---|
| Apple, Inc. USBDevice（Apple Mobile Device USB Composite Device / USB Device，涵盖 iPhone 的 PID 0x12a0–0x12af 等） | 423.206.4.0（2018-08） | ARM64 |
| 同上 | 486.0.0.0（2020-10） | x86 / AMD64 / ARM64 |
| 同上 | 538.0.0.0（2023-06） | AMD64 / ARM64 |
| 同上 | **552.0.0.0（2025-04-25，“Apple, Inc. USBDevice Driver Update”）** | **ARM64** |
| Apple Mobile Device Ethernet（netaapl，“Apple - Net - 1.8.5.1”，2013） | 1.8.5.1 | **只有 X86 / AMD64，没有 ARM64** |

- 查询方式：`https://www.catalog.update.microsoft.com/Search.aspx?q=Apple%20Inc.%20USB` 和 `...?q=Apple%20Mobile%20Device%20Ethernet`，再用 `ScopedViewInline.aspx?updateid=<id>` 看架构和硬件 ID。
  - 552 ARM64 那一条的 updateid 是 `33ca8d73-5ef6-45d2-94be-1198e7a74ddc`，硬件 ID 包含 `USB\VID_05AC&PID_12A0…12A8` 等。
- Apple Devices 的 Store 包 1.1540.24088.0 包含 arm64 架构，最低系统 Windows 11 24H2（26100）。这是 Store catalog 的元数据，我没有拆包确认服务进程本身是不是原生 ARM64。不过用户态服务就算是 x64，也能在 Prism 下以仿真方式运行；内核驱动不能仿真，但它已经有 ARM64 原生版。

### 3.2 用户报告

- Snapdragon X Elite X1E80100，Win11 24H2 26100，装了 iTunes 后，设备管理器里正常出现 “Apple Mobile Device USB Composite Device” 和 “Apple Mobile Device USB Device”，**只是没有 Apple Mobile Device Ethernet**，所以 USB 共享上网失败（2025-06）。这与 catalog 一致：usbaapl 有 ARM64 版，netaapl 没有。
  https://learn.microsoft.com/en-us/answers/questions/3863362/apple-usb-tethering-is-not-working-on-my-windows11
- Surface Laptop 7（ARM）：iTunes 能识别 iPhone，但网络设置里没有 Ethernet，USB 共享上网失败（2025-04 至 06）。
  https://discussions.apple.com/thread/256047762
- 德语社区，ARM64 PC，Win11 26H1，iPhone 16，只装了 Apple Devices（2026-07）：一开始 iPhone 显示黄色感叹号（Code 1），AMDS 起不来；**一次 Windows 更新后，Apple Devices 能识别 iPhone 了**。这说明 ARM64 上可用，但依赖 Windows Update 推送驱动，而且 26H1 这类新版本早期可能有问题。
  https://communities.apple.com/de/thread/256330140

### 3.3 ARM64 结论

- **usbmux 路线（我们的设计）在 ARM64 上可行**：
  - USB 驱动 usbaapl 有官方 ARM64 WHQL 版，通过 Windows Update 自动安装。
  - Apple Devices 有 arm64 包，最低 Win11 24H2。
  - 有用户报告确认 ARM64 上 iTunes / Apple Devices 能识别 iPhone。
- **USB 网卡路线在 ARM64 上不可行**：netaapl（Apple Mobile Device Ethernet）没有 ARM64 版，内核驱动也没法仿真。
- 剩余风险：
  - 需要 24H2 或更新的系统，才能装 arm64 版 Apple Devices。23H2 的 ARM 机器只能装 x64 MSI 或 iTunes，驱动仍然来自 Windows Update，但这个组合没有找到实测报告。
  - Windows Update 推驱动可能延迟，或者被企业策略禁用。
  - 26H1 早期出过问题。
  - **我们手上没有 Snapdragon 实机验证**。上线前必须找一台 Copilot+ PC 跑一遍 ListDevices、Connect 和 diagnostics_relay。

## 4. USB 网络

- Windows 上的 “Apple Mobile Device Ethernet” 由 netaapl 驱动提供（netaapl64.sys，2013 年的 1.8.5.1）。iTunes 或 AMDS 的安装包会带上它，Windows Update 也能装，但只有 x86/x64。它对应 iPhone 的“个人热点” USB 接口，要等手机端开启个人热点时才有链路。我们的 App 不开个人热点，所以这块网卡不会承载我们的流量。（根据驱动的用途和上面几条 ARM 报告推断；没有找到 Apple 关于“非热点时网卡状态”的官方说明。）
- Linux 上 ipheth 会在插线时出现，是因为开源 usbmuxd 会切到带网络接口的 USB 配置，链路只在热点开启时才 up。在 CellGlide 里它只用于把 USB 共享网卡排除出上游选择。
- 结论：**Windows 版不需要 netaapl**。usbmux-only 设计的数据和控制都走 usbaapl 加 AMDS 的 27015。线缆延迟改用 usbmux 通道上的往返时间测量；设备检测用 usbmux 的 `Listen` 事件，可选再用 SetupAPI 看 VID 05AC。这样也顺带避开了 ARM64 没有 netaapl 的问题。
- Windows 上的 USB 速率（对应 `usb_speed.rs`）需要另做：用 SetupAPI 或 `IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX(_V2)` 查询。这是锦上添花，不影响主流程。

## 5. AMDS 不可用时的替代方案

### 5.1 自带 usbmux（WinUSB / libusb）

- 技术上可行：libimobiledevice 的 usbmuxd 本身支持 Windows（README：“Tested on Linux, macOS, Windows and Android”），但需要 libusb，而 libusb 在 Windows 上要求设备接口绑定 WinUSB、libusbK 或 libusb0。
  https://github.com/libimobiledevice/usbmuxd
- 障碍：
  - iPhone 没有 MS OS 描述符（WinUSB compatible ID），Windows 不会自动绑 WinUSB。如果装了 Apple 驱动，usbaapl 会占用整个复合设备；如果没装，iPhone 只会以 WPD/MTP（相机）设备出现，usbmux 接口没有驱动。
  - 要绑定 WinUSB，就得装一个 INF 驱动包。即使只引用系统自带的 WinUSB，Windows 也要求驱动包有签名的 .cat：x64 / ARM64 默认强制驱动签名，自签名只能走 Zadig 或 libwdi 那种往“受信任根证书”塞自签证书的做法。没有代码签名证书时这条路基本走不通，在 EV 证书加 Microsoft attestation 签名的要求下更不可行。
  - Zadig 式替换会把 usbaapl 换掉，导致 iTunes、Apple Devices、iMazing 等全部失效，和用户现有的 Apple 软件冲突。
  - 还得自己实现 USB 配置切换、usbmux 的 TCP-over-USB 协议栈、配对和 pair record 存储（Pair、SavePairRecord 的那套逻辑），工作量和风险都大。
  - 法律层面：usbmux 和 lockdown 协议是反向工程的产物，libimobiledevice 是 LGPL。自己实现或静态链接要注意许可证；调用 Apple 已安装的服务，比分发 Apple 组件更安全。不建议重新分发 Apple 的 MSI 或驱动（需要 Apple 授权）。iMazing 的做法是让安装程序从 Microsoft Store 拉 Apple 组件。

### 5.2 推荐方案（一行 PowerShell 安装，无代码签名证书）

1. **依赖 Apple 官方组件，不带任何驱动。** 客户端是纯用户态 exe 加 Wintun（Wintun.dll 本身已由 WireGuard LLC 签名）。
2. 安装脚本检测 `Apple Mobile Device Service` 的注册表键和服务状态，以及 27015 是否可连：
   - **已有（iTunes / Apple Devices / MSI 任意一种）**：直接使用。
   - **没有**：引导用户安装 Apple Devices。可以选择让脚本执行 `winget install --id 9NP83LWLPZ9K --source msstore`，但同意 Store 协议应当由用户本人确认，不要默认加 `--accept-*`。x64 的 Win10 或旧版本也可以用 `winget install Apple.AppleMobileDeviceSupport`（需要管理员，装 x64 MSI）。
   - **ARM64**：要求 Win11 24H2 及以上，优先装 Store 版 Apple Devices（arm64 包）。提示用户插线后让 Windows Update 完成驱动安装；设备管理器里如果有感叹号，去“可选更新 / 驱动程序更新”里装 Apple 驱动。
3. 不实现 WinUSB 或自带 usbmuxd，至少第一期不做。
4. 不依赖 netaapl 或 USB 网卡。
5. 没有代码签名证书时，脚本用 `Invoke-WebRequest` 下载、解压到 `%LOCALAPPDATA%`。这条路径通常不会触发 SmartScreen 的 MOTW 拦截，但杀软误报和企业策略仍是风险；服务化运行（TUN）需要管理员权限，这和 Apple 组件无关。

## 6. “信任此电脑”提示怎么触发

- Linux 的开源 usbmuxd 在 `preflight.c` 里，设备接入后会自动调用 `lockdownd_pair()`。如果没配对过，它会主动弹出信任提示，订阅 `com.apple.mobile.lockdown.request_pair`，用户点“信任”后完成配对并存下记录。所以在 Linux 上我们只需要 ReadPairRecord。
  https://github.com/libimobiledevice/usbmuxd/blob/master/src/preflight.c
- Windows 上的 AMDS：Apple 官方步骤是“打开 Apple Devices app，再连接设备，看到提示点信任”（support.apple.com/108643、/109054）。没有一手资料证明 AMDS 服务本身在插线时会主动配对。普遍的经验是提示由 Apple Devices 或 iTunes 这类 MobileDevice 客户端在访问设备时触发；上面德语帖里，Store 版装好后提示也没有自动出现。**这一点没有确认，必须实机验证。**
  https://support.apple.com/en-us/108643
  https://support.apple.com/en-nz/109054
- 对我们的影响和建议：
  - 最简单的做法：首次使用时提示用户“打开 Apple Devices 并在 iPhone 上点信任”。配对记录由 AMDS 写进 Lockdown 目录，之后我们用 ReadPairRecord 就能读到，与 Apple 软件共用同一份记录。
  - 更稳的做法：客户端自己实现配对。用 usbmux 发 `ReadBUID` 拿 SystemBUID，生成 host / root 证书和密钥，向 lockdown 发 `Pair`（iOS 会弹出信任提示，锁屏时返回 PasswordProtected，需要轮询或等通知），成功后用 usbmux 的 `SavePairRecord` 交给 AMDS 保存。go-ios（ios/pair_save.go）和 libimobiledevice 都是这么做的。这样 Linux 上 usbmuxd 不自动配对的情况（比如 preflight 被关掉）也能覆盖，但需要新增 ReadBUID、SavePairRecord、Pair 三项能力，并处理 iOS 17 以上的 `HostName`、`WiFiMACAddress` 等字段。

## 7. 开放风险汇总

1. ARM64：没有实机验证。Apple Devices 的 arm64 包需要 24H2；驱动靠 Windows Update，有延迟或被策略禁用的风险；26H1 早期出过问题。
2. 信任提示：不确定 AMDS 会不会自动弹出，可能需要用户先打开 Apple Devices，或者我们自己实现 Pair。
3. Lockdown 目录的 ACL 没有一手资料，不能把读文件当成主路径。
4. Store 版 Apple Devices 和 iTunes 的 AMDS 行为有差别（Wi-Fi 发现），USB 路径没有发现差异；以后 Apple 改端口或改用命名管道的可能性小但存在，检测逻辑要以 TCP 27015 为准并给出清楚的报错。
5. 不能重新分发 Apple 组件，只能引导用户从 Store 或 winget 安装。
