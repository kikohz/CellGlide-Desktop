# CellGlide Desktop

English | [中文](#中文)

The Linux and Windows clients for CellGlide: plug in your iPhone and go online over its cellular connection. A shared Rust core, with an iced interface on Linux and a WinUI 3 interface on Windows.

The iPhone app and the Mac client live elsewhere; the phone protocol follows the iPhone app.

## Install

Supported: Debian / Ubuntu (deb) and Fedora 42 or newer (rpm), on amd64 / x86_64 and arm64 / aarch64, with systemd and glibc 2.35 or newer. RHEL / AlmaLinux / Rocky 9 (glibc 2.34) are not supported; RHEL 10 family (glibc 2.39) should work but is untested.

### Debian / Ubuntu

One step (adds the repository and installs the package):

```bash
curl -fsSL https://pkg.cellglide.com/setup-apt.sh | sudo sh
```

Or set it up by hand with the three commands in the Chinese section above: keyring to `/etc/apt/keyrings/cellglide-archive-keyring.gpg`, the sources file `/etc/apt/sources.list.d/cellglide.sources`, then `sudo apt update && sudo apt install cellglide`.

Debian does not ship systemd-resolved by default. Without it CellGlide cannot manage DNS, so install it first: `sudo apt install systemd-resolved`. Ubuntu has it.

### Fedora

```bash
sudo dnf install https://pkg.cellglide.com/rpm/x86_64/cellglide-1.0.0-1.x86_64.rpm
```

The package carries `cellglide.repo` and the signing key, so `dnf upgrade` keeps it current. The first time you install or `dnf upgrade` through the repository, dnf asks whether to import the CellGlide signing key (fingerprint `71082C14C8979B6C13E1B1D728A16C8C0A0D2A98`); check it and accept. On arm64 use `rpm/aarch64/cellglide-1.0.0-1.aarch64.rpm`. Alternatively write `cellglide.repo` and fetch `RPM-GPG-KEY-cellglide` as shown above, then `sudo dnf install cellglide`. Fedora ships systemd-resolved as a separate package; without it CellGlide cannot manage DNS, so `sudo dnf install systemd-resolved`, and `sudo systemctl enable --now systemd-resolved` only if it is installed but not running.

### After installing

- Installing adds the user who ran `sudo` to the `cellglide` group. Log out and back in before using `cellglide` or `cellglide-gui`. For another user: `sudo usermod -aG cellglide <user>`.
- Installing through GNOME Software, KDE Discover or another PackageKit front end doesn't tell the package which user to add, so nobody is added to the group. Run `sudo usermod -aG cellglide $USER` afterwards, then log out and back in.
- The service runs as the dedicated `cellglide` user and starts at boot. `cellglide doctor` checks the setup and prints a fix for each problem.

### Upgrading from a beta

If you installed a beta, upgrade through the repository: `sudo apt update && sudo apt upgrade` on Debian / Ubuntu, or `sudo dnf upgrade --refresh` on Fedora. Log out and back in, or reboot, once afterwards.

### Uninstall

- deb: `sudo apt remove cellglide` keeps the settings in `/var/lib/cellglide` and the repository files `/etc/apt/sources.list.d/cellglide.sources` and `/etc/apt/keyrings/cellglide-archive-keyring.gpg`, so `apt update` keeps working; `sudo apt purge cellglide` deletes all of them.
- rpm: `sudo dnf remove cellglide`. rpm has no purge, so `/var/lib/cellglide` stays; delete it by hand if you do not want the settings.
- Both leave the `cellglide` system user and group in place.

## Windows

Supported: Windows 11 22H2 or newer on x64. ARM64 needs Windows 11 24H2 or newer and is a **test build, not yet tested on real hardware**.

### Before installing

Install Apple's **Apple Devices** app from the Microsoft Store: <https://apps.microsoft.com/detail/9NP83LWLPZ9K>. It brings the driver and the service CellGlide uses to talk to the iPhone. If it is missing, the installer explains why and offers to install it for you (through winget; you accept the Store terms there yourself).

### Install

Open an ordinary (not administrator) PowerShell window and run:

```powershell
irm https://pkg.cellglide.com/install.ps1 | iex
```

Windows asks once for administrator rights (UAC). The installer downloads the package for your PC's architecture, checks its SHA-256 against `SHA256SUMS`, installs the service and the app, starts CellGlide and prints the result of `cellglide doctor`. The window stays open whether it succeeds or fails.

### Install from the zip

1. From the CellGlide website (<https://cellglide.com>), download `CellGlide-<version>-windows-x64.zip` (or `-arm64.zip`) and `install.ps1` into one folder, and compare the zip's SHA-256 with the one the website shows.
2. In PowerShell, in that folder, clear the "downloaded from the internet" mark on both files, then run the installer:

```powershell
Unblock-File .\install.ps1, .\CellGlide-<version>-windows-x64.zip
powershell -ExecutionPolicy Bypass -File .\install.ps1 -ZipPath .\CellGlide-<version>-windows-x64.zip
```

The installer prints the zip's SHA-256 before installing; it must match the website's.

### Upgrade

Run the one-line command again. It replaces the program as a whole and keeps your settings; a failed upgrade puts the previous version back.

### Uninstall

- **Settings › Apps › Installed apps › CellGlide › Uninstall**, or in PowerShell: `irm https://pkg.cellglide.com/uninstall.ps1 | iex`. Both keep your settings and history.
- To delete the data as well, download the script and run it with `-Purge`, from an ordinary (not administrator) window:

```powershell
irm https://pkg.cellglide.com/uninstall.ps1 -OutFile uninstall.ps1
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -Purge
```

This deletes `C:\ProgramData\CellGlide` and your own `%LOCALAPPDATA%\CellGlide`. Other accounts on the PC keep their own `%LOCALAPPDATA%\CellGlide`. Run from an administrator window, the script never deletes `%LOCALAPPDATA%\CellGlide` and says so; delete that folder yourself, or rerun with `-Purge` from an ordinary window.

- Uninstalling removes the "start at sign-in" entry of every user who is signed in. A user who is not signed in at that moment (and a Microsoft / Entra ID account, even when signed in) keeps an entry that points to the deleted program. It does nothing; remove it in **Settings › Apps › Startup** if you like.
- Apple Devices is not touched.

### Where things are

```
C:\Program Files\CellGlide\    cellglide-service.exe, cellglide.exe (command line), wintun.dll,
                               THIRD-PARTY-LICENSES.txt, uninstall.ps1, app\ (the CellGlide app)
C:\ProgramData\CellGlide\      settings.json, health\, logs\
%LOCALAPPDATA%\CellGlide\      app preferences, speed test history
```

The command line is not added to `PATH`: run it as `& "C:\Program Files\CellGlide\cellglide.exe" doctor`.

### Unsigned builds

CellGlide for Windows is not code-signed yet.

- With **Smart App Control** in enforcement mode, Windows blocks unsigned programs, so CellGlide cannot be installed; the installer stops without changing anything and says so.
- Files downloaded with a browser carry the "from the internet" mark; clear it with `Unblock-File` as shown above, or use the one-line command, which is not affected.
- Microsoft Defender may report CellGlide as a false positive. Every release is submitted to Microsoft for analysis.

### Strict DNS

Strict DNS is on by default (Settings › Strict DNS). While connected, DNS queries can only go through the phone. Names only your router knows (such as `*.lan`) won't resolve, and a name that doesn't exist takes about 12 seconds to fail. Turn it off if you need those.

### Microsoft components

CellGlide includes Microsoft Windows App SDK and Windows SDK components that are licensed to you under the Microsoft Software License Terms reproduced in `THIRD-PARTY-LICENSES.txt` (the Windows App SDK terms and the Windows SDK terms); by installing or using CellGlide you agree to those terms for those components.

## Development

- Fetch the submodule first (hev-sys builds hev-socks5-tunnel from it): `git submodule update --init --recursive`.
- On macOS: `cargo test --workspace`; for the Linux-only parts, `scripts/linux-test.sh` (needs Docker / colima).
- Tests that need root and systemd-resolved run only in CI (the `privileged` job).
- Install a development build on a Linux machine with `scripts/install-dev.sh`; remove it with `scripts/install-dev.sh --uninstall`.

### Windows

Run these from the repository root in Windows PowerShell; start each session with `. scripts\dev-env.ps1`. The app itself (C#, WinUI 3) is in `windows\`.

- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-windows-core.ps1`: builds `cellglide_native.dll`, builds and tests the C# side, checks the license lists and runs the Pester tests of the Windows scripts.
- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Arch x64` (or `arm64`): builds the release zip and `SHA256SUMS.<arch>` into `dist\windows\<version>\`.
- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1`: builds this PC's zip and installs it with the real installer; `-Uninstall` removes it. Take network snapshots (`scripts\network-snapshot.ps1`) before and after.

### Crates

| Crate | Purpose |
|---|---|
| `cg-protocol` | Wire formats shared with the iPhone app and usbmuxd; no IO |
| `cg-ipc` | Messages between the service and its clients (one JSON object per line), and the user-facing state descriptions |
| `cg-i18n` | User-facing text in Simplified Chinese and English (`locales/*.yml`) |
| `cg-core` | Platform-neutral core: talks to usbmuxd and the phone, relays traffic, answers DNS, runs the connection state machine |
| `cg-platform` | Platform tunnel backends (Linux and Windows) |
| `hev-sys` | FFI to and build of hev-socks5-tunnel (submodule) |
| `cg-netstack` | The forwarding engine used on Windows in place of hev-socks5-tunnel, with the same settings |
| `cg-ffi` | `cellglide_native.dll`: everything the Windows app needs except drawing, behind a small C interface |
| `cg-tools` | Tools that run in the user's own processes: speed test, DNS leak test and more |
| `cg-diag` | The diagnostics bundle: format, redaction and upload |
| `cg-service` | The system service `cellglide-service`; also carries the deb / rpm packaging metadata |
| `cg-cli` | The `cellglide` command line |
| `cg-gui` | The `cellglide-gui` window and tray |

## License

This project is released under the MIT License; see [LICENSE](LICENSE). Bundled third-party code (Rust dependencies, hev-socks5-tunnel and its C submodules) keeps its own licenses, listed in `crates/cg-gui/assets/third-party-licenses.txt` (installed as `THIRD-PARTY-LICENSES.txt`).

On Windows, `THIRD-PARTY-LICENSES.txt` in the zip (and the About page) is put together from `packaging/licenses/windows-rust.txt` (Rust dependencies; no hev or C code on Windows) and `packaging/licenses/nuget-notices.txt` (the Microsoft terms notice, then .NET, Windows App SDK and the other shipped packages, each with its full text); Wintun's license is `wintun-LICENSE.txt` next to `wintun.dll`.

---

## 中文

CellGlide 的 Linux 与 Windows 客户端：插上 iPhone，经手机的蜂窝网络上网。Rust 共用核心，Linux 界面用 iced，Windows 界面用 WinUI 3。

iPhone App 与 Mac 客户端不在本仓库，手机协议以 iPhone App 为准。

## 安装

支持 Debian / Ubuntu（deb）和 Fedora 42 及以上（rpm），amd64 / x86_64 与 arm64 / aarch64。需要 systemd 和 glibc 2.35 或更新，所以 RHEL / AlmaLinux / Rocky 9（glibc 2.34）不支持；RHEL 10 系（glibc 2.39）理论上可以，但没有测试过。

### Debian / Ubuntu

一步配置软件源并安装：

```bash
curl -fsSL https://pkg.cellglide.com/setup-apt.sh | sudo sh
```

或手动配置：

```bash
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSLo /etc/apt/keyrings/cellglide-archive-keyring.gpg https://pkg.cellglide.com/cellglide-archive-keyring.gpg
sudo tee /etc/apt/sources.list.d/cellglide.sources >/dev/null <<'EOF'
Types: deb
URIs: https://pkg.cellglide.com/apt
Suites: stable
Components: main
Signed-By: /etc/apt/keyrings/cellglide-archive-keyring.gpg
EOF
sudo apt update && sudo apt install cellglide
```

Debian 默认不带 systemd-resolved；没有它 CellGlide 不能接管 DNS，需要先装：`sudo apt install systemd-resolved`。Ubuntu 已自带。

### Fedora

直接安装（包里带有软件源配置和公钥，之后 `dnf upgrade` 会自动更新）：

```bash
sudo dnf install https://pkg.cellglide.com/rpm/x86_64/cellglide-1.0.0-1.x86_64.rpm
```

第一次通过软件源安装或 `dnf upgrade` 时，dnf 会询问是否导入 CellGlide 的签名公钥（指纹 `71082C14C8979B6C13E1B1D728A16C8C0A0D2A98`），核对后确认即可。

arm64 机器把路径换成 `rpm/aarch64/`、文件名换成 `cellglide-1.0.0-1.aarch64.rpm`。

或者先配置软件源再装：

```bash
sudo curl -fsSLo /etc/pki/rpm-gpg/RPM-GPG-KEY-cellglide https://pkg.cellglide.com/RPM-GPG-KEY-cellglide
sudo tee /etc/yum.repos.d/cellglide.repo >/dev/null <<'EOF'
[cellglide]
name=CellGlide
baseurl=https://pkg.cellglide.com/rpm/$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-cellglide
EOF
sudo dnf install cellglide
```

Fedora 的 systemd-resolved 是单独的包，没装就不能接管 DNS：`sudo dnf install systemd-resolved`；已装但没在运行时再执行 `sudo systemctl enable --now systemd-resolved`。

### 安装之后

- 安装时会把用 `sudo` 安装的那个用户加进 `cellglide` 组，**注销再登录**后才能控制 CellGlide（`cellglide status`、`cellglide-gui`）。其他用户手动加：`sudo usermod -aG cellglide <用户名>`。
- 通过 GNOME 软件、KDE Discover 等图形界面（PackageKit）安装时，安装脚本不知道是哪个用户，不会自动加组；装好后执行 `sudo usermod -aG cellglide $USER`，再注销并重新登录。
- 服务以专用用户 `cellglide` 运行，开机自启；`cellglide doctor` 检查环境，并给出每个问题的修复命令。

### 从 beta 升级

装过 beta 版的话，直接通过软件源升级：deb 用 `sudo apt update && sudo apt upgrade`，rpm 用 `sudo dnf upgrade --refresh`。升级后注销或重启一次。

### 卸载

- deb：`sudo apt remove cellglide` 删除程序，保留 `/var/lib/cellglide` 里的设置，以及 `/etc/apt/sources.list.d/cellglide.sources` 和 `/etc/apt/keyrings/cellglide-archive-keyring.gpg`（所以之后 `apt update` 照常工作）；`sudo apt purge cellglide` 连这些一起删除。
- rpm：`sudo dnf remove cellglide`。rpm 没有 purge，`/var/lib/cellglide` 会保留，不需要设置了就手动 `sudo rm -rf /var/lib/cellglide`。
- 两者都保留 `cellglide` 系统用户和组。

## Windows

支持 Windows 11 22H2 及以上的 x64 电脑。ARM64 要求 Windows 11 24H2 及以上，目前是**测试版，没有在真机上测过**。

### 安装之前

先从微软商店安装 Apple 的「Apple 设备」（Apple Devices）：<https://apps.microsoft.com/detail/9NP83LWLPZ9K>。CellGlide 靠它带来的驱动和服务与 iPhone 通信。没装的话，安装脚本会说明原因并询问是否替你安装（经 winget，商店的条款由你在 winget 里自己同意）。

### 安装

打开一个普通的（不是管理员的）PowerShell 窗口，运行：

```powershell
irm https://pkg.cellglide.com/install.ps1 | iex
```

Windows 会弹一次管理员权限确认（UAC）。安装脚本按本机架构下载安装包，用 `SHA256SUMS` 核对 SHA-256，装好服务和程序，启动 CellGlide，并打印 `cellglide doctor` 的结果。无论成功还是失败，窗口都不会被关掉。

### 直接下载 zip 安装

1. 从 CellGlide 官网（<https://cellglide.com>）把 `CellGlide-<版本>-windows-x64.zip`（ARM64 是 `-arm64.zip`）和 `install.ps1` 下载到同一个文件夹，并把 zip 的 SHA-256 与官网上写的核对。
2. 在这个文件夹里打开 PowerShell，先去掉两个文件的「来自互联网」标记，再运行安装脚本：

```powershell
Unblock-File .\install.ps1, .\CellGlide-<版本>-windows-x64.zip
powershell -ExecutionPolicy Bypass -File .\install.ps1 -ZipPath .\CellGlide-<版本>-windows-x64.zip
```

安装脚本在安装前会打印 zip 的 SHA-256，应与官网上的相同。

### 升级

再运行一次上面那行命令即可。程序整体替换，设置保留；升级中途失败时会恢复原来的版本。

### 卸载

- 在「设置 › 应用 › 已安装的应用」里找到 CellGlide 卸载，或在 PowerShell 里运行 `irm https://pkg.cellglide.com/uninstall.ps1 | iex`。两种方式都保留设置和历史记录。
- 要连数据一起删除，先下载脚本，再在普通的（不是管理员的）窗口里带 `-Purge` 运行：

```powershell
irm https://pkg.cellglide.com/uninstall.ps1 -OutFile uninstall.ps1
powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -Purge
```

这会删除 `C:\ProgramData\CellGlide` 和你自己的 `%LOCALAPPDATA%\CellGlide`；这台电脑上其他账户各自的 `%LOCALAPPDATA%\CellGlide` 保留。在管理员窗口里运行时，脚本不会删除 `%LOCALAPPDATA%\CellGlide`，并会提示：请自己删掉这个文件夹，或在普通窗口里带 `-Purge` 再运行一次。

- 卸载会删除每个已登录用户的「登录时启动」项。卸载时没有登录的用户（以及 Microsoft / Entra ID 账户，即使已登录）会留下一个指向已删除程序的启动项，它不起任何作用，可以在「设置 › 应用 › 启动」里删掉。
- 不会动「Apple 设备」。

### 文件位置

```
C:\Program Files\CellGlide\    cellglide-service.exe、cellglide.exe（命令行）、wintun.dll、
                               THIRD-PARTY-LICENSES.txt、uninstall.ps1、app\（CellGlide 程序）
C:\ProgramData\CellGlide\      settings.json、health\、logs\
%LOCALAPPDATA%\CellGlide\      界面偏好、测速历史
```

命令行没有加进 `PATH`，用完整路径运行，例如 `& "C:\Program Files\CellGlide\cellglide.exe" doctor`。

### 没有代码签名

Windows 版暂时没有代码签名。

- 开着「智能应用控制」（Smart App Control）且处于强制模式时，Windows 会拦下未签名的程序，所以装不了 CellGlide；安装脚本会说明原因并停止，不改动任何东西。
- 用浏览器下载的文件带有「来自互联网」标记，要像上面那样用 `Unblock-File` 去掉；用一行命令安装不受影响。
- Microsoft Defender 可能误报。每个版本发布前都会提交给微软分析。

### 严格 DNS

严格 DNS 默认开启（设置 › 严格 DNS）。连接期间，DNS 查询只能经过手机。只有路由器认识的名字（例如 `*.lan`）解析不了，不存在的名字要约 12 秒才返回失败。需要时可以关掉。

### 微软组件

CellGlide 包含微软的 Windows App SDK 和 Windows SDK 组件，它们按 `THIRD-PARTY-LICENSES.txt` 中收录的 Microsoft 软件许可条款（Windows App SDK 条款与 Windows SDK 条款）授权给你；安装或使用 CellGlide，即表示你就这些组件同意这些条款。

## 开发

- 先取子模块（hev-sys 要用它编译 hev-socks5-tunnel）：`git submodule update --init --recursive`。
- 在 macOS 上开发：`cargo test --workspace`；Linux 专有部分：`scripts/linux-test.sh`（需要 Docker / colima）。
- 需要 root 与 systemd-resolved 的测试只在 CI（`privileged` 任务）里跑。
- 在 Linux 机器上安装开发版：

```bash
scripts/install-dev.sh
```

- 常用命令：`cellglide status`、`cellglide connect`、`cellglide disconnect`、`cellglide doctor`、`cellglide watch`、`cellglide-gui`。
- 卸载：`scripts/install-dev.sh --uninstall`。

### Windows

在仓库根目录用 Windows PowerShell 运行，每个会话先 `. scripts\dev-env.ps1`。程序本身（C#、WinUI 3）在 `windows\`。

- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-windows-core.ps1`：编译 `cellglide_native.dll`，编译并测试 C# 部分，检查许可清单，跑 Windows 脚本的 Pester 测试。
- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Arch x64`（或 `arm64`）：在 `dist\windows\<版本>\` 里生成正式的 zip 和 `SHA256SUMS.<架构>`。
- `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1`：编出本机架构的 zip，用正式的安装脚本装上；`-Uninstall` 卸载。前后各拍一次网络快照（`scripts\network-snapshot.ps1`）。

### Crate 一览

| Crate | 作用 |
|---|---|
| `cg-protocol` | 与 iPhone App、usbmuxd 共用的线格式，不做 IO |
| `cg-ipc` | 服务与客户端之间的消息（每行一个 JSON），以及给用户看的状态描述 |
| `cg-i18n` | 简体中文与英文的界面文字（`locales/*.yml`） |
| `cg-core` | 平台无关的核心：usbmuxd 与手机通信、转发流量、应答 DNS、连接状态机 |
| `cg-platform` | 平台隧道后端（Linux 与 Windows） |
| `hev-sys` | hev-socks5-tunnel 的 FFI 与构建（子模块） |
| `cg-netstack` | Windows 上代替 hev-socks5-tunnel 的转发引擎，设置与 hev 相同 |
| `cg-ffi` | `cellglide_native.dll`：Windows 程序除了绘制界面以外需要的一切，经一个小的 C 接口提供 |
| `cg-tools` | 在用户自己的进程里跑的工具：测速、DNS 泄漏检测等 |
| `cg-diag` | 诊断包：格式、脱敏与上传 |
| `cg-service` | 系统服务 `cellglide-service`，也挂着 deb / rpm 打包元数据 |
| `cg-cli` | 命令行 `cellglide` |
| `cg-gui` | 图形界面与托盘 `cellglide-gui` |

## 许可证

本项目以 MIT 许可证发布，见 [LICENSE](LICENSE)。随附的第三方代码（Rust 依赖、hev-socks5-tunnel 及其 C 子模块）保留各自的许可证，清单见 `crates/cg-gui/assets/third-party-licenses.txt`（安装包里是 `THIRD-PARTY-LICENSES.txt`）。

Windows 版 zip 里的 `THIRD-PARTY-LICENSES.txt`（以及关于页）由 `packaging/licenses/windows-rust.txt`（Rust 依赖；Windows 版没有 hev 和 C 代码）和 `packaging/licenses/nuget-notices.txt`（开头是微软条款声明，然后是 .NET、Windows App SDK 等随包分发的组件，各附许可全文）拼成；Wintun 的许可是 `wintun.dll` 旁边的 `wintun-LICENSE.txt`。
