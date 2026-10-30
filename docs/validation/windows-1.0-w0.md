# Windows 1.0 W0 验证记录

## 机器与工具链

| 项目 | 值 |
| --- | --- |
| 系统 | Windows 11 专业版 23H2 (22631) x64 |
| CPU | 11th Gen Intel Core i7-1195G7 @ 2.90GHz |
| rustc | 1.99.0 (b940084d7 2026-09-28)，由 rust-toolchain.toml 固定 |
| Rust targets | x86_64-pc-windows-msvc、aarch64-pc-windows-msvc |
| clang | 19.1.5（VS 2022 BuildTools 的 Clang 组件，`VC\Tools\Llvm\x64\bin`） |
| MSVC | 14.44.35207 |
| .NET SDK | 10.0.403 |
| gh | 2.103.3（`C:\Program Files\GitHub CLI\gh.exe`），已登录 kikohz |

开发环境：每个新 PowerShell 会话先运行 `. scripts\dev-env.ps1`（把 cargo 和 clang 加到 PATH）。
clang 来自 `setup.exe modify --add Microsoft.VisualStudio.Component.VC.Llvm.Clang`；ring 的构建脚本编译 ARM64 目标时需要它。

## Task 1：编译基线

### cargo check

- `cargo check --workspace --locked`（x86_64）：Finished。
- `cargo check --workspace --locked --target aarch64-pc-windows-msvc`：Finished（安装 clang 之前因 ring 找不到 clang 失败）。

### 测试代码编译状态（Task 8 的清零清单）

当前 cargo 不支持 `--keep-going`，改为逐个 crate 运行 `cargo test -p <crate> --locked --no-run`。
只有 cg-core（及依赖其 testing 模块的 cg-service）测试编译失败，其余 crate 都能编译：

| 位置 | 错误 |
| --- | --- |
| crates/cg-core/src/testing/fake_mux.rs:9:29 | E0432 `tokio::net::UnixListener`、`UnixStream` 在 Windows 上不存在 |
| crates/cg-core/src/testing/fake_mux.rs:51:22 | E0599 `MuxEndpoint::Unix` 变体不存在 |
| crates/cg-core/src/mux.rs:231:33 | E0599 `MuxEndpoint::Unix` 变体不存在 |
| crates/cg-core/src/controller.rs:1769:26 | E0599 `MuxEndpoint::Unix` 变体不存在 |

cg-service 的测试编译失败同样来自 fake_mux.rs 的前两项（经 cg-core 的 testing 模块）。
这是编译器在第一阶段报出的错误；修掉后可能暴露更多，Task 8 完成时需重新运行并更新本节。

### GitHub

`gh` 已安装（2.103.3，`C:\Program Files\GitHub CLI\gh.exe`），`gh auth status` 显示已登录 github.com 账号 kikohz（https，scopes: gist、read:org、repo）。`gh auth setup-git` 已运行，`git push origin windows` 可用。（本会话的 PATH 过期曾导致误判未安装。）

## Task 3：wintun

### wintun.dll 来源

- 下载地址 `https://www.wintun.net/builds/wintun-0.14.1.zip`。
- SHA2-256 照抄自 https://www.wintun.net/ 下载链接旁的公布值（2026-10-29 抄录）：
  `07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51`，已写死在 `scripts/fetch-wintun.ps1`。下载后 `Get-FileHash` 一致。
- 另外核对了 zip 里 `bin\amd64\wintun.dll` 的 Authenticode 签名：`Get-AuthenticodeSignature` 为 `Valid`，签名者 `CN=WireGuard LLC`（颁发者 DigiCert EV Code Signing CA (SHA2)，证书指纹 `DF98E075A012ED8C86FBCF14854B8F9555CB3D45`）。
- 脚本把 DLL 复制为 `wintun.dll`，许可证复制为 `wintun-LICENSE.txt`（与设计 §5.2 的安装目录命名一致），默认目标 `target\debug`、`target\debug\deps`、`target\debug\examples`。下载物只在 `target\wintun\`。

### 加载

- `LoadLibraryExW` 带 `LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`；debug 版先看 `CELLGLIDE_WINTUN_DLL`（必须是绝对路径）。
- 找不到时的错误：`can't load wintun.dll from C:\nonexistent\wintun.dll (The specified module could not be found. (os error 126)); it should be in the same directory as cellglide-service.exe`。

### 特权测试（管理员）

`cargo test -p cg-platform --test windows_privileged --locked -- --ignored --test-threads=1`：3 个测试全部通过。

| 测试 | 结果 |
| --- | --- |
| adapter_appears_and_disappears | 创建后 `Get-NetAdapter -IncludeHidden -Name cellglide0` 能查到；drop 后第一次查询就查不到（约 0.63 s，基本是启动 powershell 的时间） |
| open_finds_an_existing_adapter | `Adapter::open` 返回 Some，LUID 与创建的一致；两者 drop 后 `open` 立即返回 None（< 1 ms） |
| session_starts_receives_and_sends | 4 MiB 会话可启动，读事件句柄有效，`try_receive` 正常，发送 20 字节 IPv4 包成功，空包被拒绝 |

前后快照 `-Compare`：`identical`。

### 崩溃清理（`wintun_probe` + `taskkill /F`）

- 运行中：`Get-NetAdapter` 显示 `cellglide0`，`InterfaceDescription` 为 `CellGlide Tunnel`，状态 Up；`pnputil /enum-devices /class Net` 里是 `SWD\Wintun\{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}`，Status `Started`。
- `taskkill /F` 之后：`Get-NetAdapter -IncludeHidden` 约 40 ms 内就查不到，`Get-NetIPInterface -InterfaceAlias cellglide0` 为空，快照对比 `identical`。
- **与预期不同**：`pnputil /enum-devices /class Net` 里**仍有**这一项，Status `Disconnected`；`Get-PnpDevice` 显示 `Present=False`、`Problem=CM_PROB_PHANTOM`。也就是说，网卡和它的 IP 接口随进程消失，但 PnP 设备记录以「不存在的设备」形式留下（同拔掉的 USB 设备）。`pnputil /enum-devices /connected` 里没有它。
- 这个残留记录：`WintunOpenAdapter("cellglide0")` 对它返回「找不到」（`Adapter::open` 为 None）；下一次正常的 create + close 之后设备记录完全消失（`Get-PnpDevice` 查无此项）。因为 GUID 固定，残留最多只有这一条，不会累积。
- 正常关闭（drop / `WintunCloseAdapter`）不留任何设备记录。
- 结论：崩溃后不需要额外清理网络配置；PnP 里的不存在设备记录在下次服务启动建网卡并关闭时被替换掉。是否要在卸载脚本里删掉这条记录，留给安装脚本任务决定（不在本任务里动）。

### 同名同 GUID 第二个实例

第一个 `wintun_probe` 持有网卡时再运行第二个：

- 第二个实例的 `WintunOpenAdapter("cellglide0")` **成功**（拿到同一 LUID `0x35008001000000`）。
- 它的 `WintunCreateAdapter("cellglide0", "CellGlide", 固定 GUID)` **失败**，last error 是 HRESULT `0x800700B7`（`HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`）而不是 Win32 码。封装里已把 `0x8007xxxx` 形式还原为 Win32 码，现在报 `os error 183`，`ErrorKind::AlreadyExists`。
- 第二个实例退出（关闭它打开的句柄）不影响第一个：第一个仍在运行，网卡仍 Up。杀掉第一个后网卡消失，快照对比 `identical`。

### 其他

- 本机另有一个不属于我们的 wintun 设备 `SWD\Wintun\{37217669-42DA-4657-A55B-0D995D328250}`（`Tailscale Tunnel`，正在运行，Present=True），全程未触碰。
- 所有快照对比（特权测试、两次崩溃实测、清理）均为 `identical`。
