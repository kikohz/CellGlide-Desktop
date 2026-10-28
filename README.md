# CellGlide Desktop

CellGlide 的 Linux 客户端：插上 iPhone，经手机的蜂窝网络上网。Rust 共用核心，界面用 iced。

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

## 许可证

本项目以 MIT 许可证发布，见 [LICENSE](LICENSE)。随附的第三方代码（Rust 依赖、hev-socks5-tunnel 及其 C 子模块）保留各自的许可证，清单见 `crates/cg-gui/assets/third-party-licenses.txt`（安装包里是 `THIRD-PARTY-LICENSES.txt`）。

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

### Crate 一览

| Crate | 作用 |
|---|---|
| `cg-protocol` | 与 iPhone App、usbmuxd 共用的线格式，不做 IO |
| `cg-ipc` | 服务与客户端之间的消息（每行一个 JSON），以及给用户看的状态描述 |
| `cg-i18n` | 简体中文与英文的界面文字（`locales/*.yml`） |
| `cg-core` | 平台无关的核心：usbmuxd 与手机通信、转发流量、应答 DNS、连接状态机 |
| `cg-platform` | 平台隧道后端（只实现了 Linux） |
| `hev-sys` | hev-socks5-tunnel 的 FFI 与构建（子模块） |
| `cg-tools` | 在用户自己的进程里跑的工具：测速、DNS 泄漏检测等 |
| `cg-diag` | 诊断包：格式、脱敏与上传 |
| `cg-service` | 系统服务 `cellglide-service`，也挂着 deb / rpm 打包元数据 |
| `cg-cli` | 命令行 `cellglide` |
| `cg-gui` | 图形界面与托盘 `cellglide-gui` |

## Development

- Fetch the submodule first (hev-sys builds hev-socks5-tunnel from it): `git submodule update --init --recursive`.
- On macOS: `cargo test --workspace`; for the Linux-only parts, `scripts/linux-test.sh` (needs Docker / colima).
- Tests that need root and systemd-resolved run only in CI (the `privileged` job).
- Install a development build on a Linux machine with `scripts/install-dev.sh`; remove it with `scripts/install-dev.sh --uninstall`.

### Crates

| Crate | Purpose |
|---|---|
| `cg-protocol` | Wire formats shared with the iPhone app and usbmuxd; no IO |
| `cg-ipc` | Messages between the service and its clients (one JSON object per line), and the user-facing state descriptions |
| `cg-i18n` | User-facing text in Simplified Chinese and English (`locales/*.yml`) |
| `cg-core` | Platform-neutral core: talks to usbmuxd and the phone, relays traffic, answers DNS, runs the connection state machine |
| `cg-platform` | Platform tunnel backends (only Linux is implemented) |
| `hev-sys` | FFI to and build of hev-socks5-tunnel (submodule) |
| `cg-tools` | Tools that run in the user's own processes: speed test, DNS leak test and more |
| `cg-diag` | The diagnostics bundle: format, redaction and upload |
| `cg-service` | The system service `cellglide-service`; also carries the deb / rpm packaging metadata |
| `cg-cli` | The `cellglide` command line |
| `cg-gui` | The `cellglide-gui` window and tray |

## License

This project is released under the MIT License; see [LICENSE](LICENSE). Bundled third-party code (Rust dependencies, hev-socks5-tunnel and its C submodules) keeps its own licenses, listed in `crates/cg-gui/assets/third-party-licenses.txt` (installed as `THIRD-PARTY-LICENSES.txt`).
