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
| gh | 未安装（GitHub 登录由用户处理，见下） |

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

`gh` 未安装，`gh auth status` 不可用。需要用户运行 `winget install GitHub.cli`、`gh auth login`、`gh auth setup-git`，之后才能推送 `windows` 分支和看 CI。
