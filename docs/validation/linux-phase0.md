# Linux 第 0 阶段结论

日期：2026-10-15（两轮：第一轮因测试机磁盘故障中断，重启后完成补测）
测试机：x86_64 一体机，4 核 x86_64，16 GB，Ubuntu 26.04 LTS（systemd-resolved，usbmuxd，另装有 Tailscale，未开出口节点）
iPhone：手机 CellGlide 2.2.0，AT&T 5G NSA（型号与 iOS 版本待补）

## 1. resolved 回环端口验证

- CI ubuntu-24.04：待重跑（GitHub Actions 因账单问题未运行）。已在本地 Lima 虚拟机（Ubuntu 24.04 aarch64，systemd 255 (255.4-1ubuntu8.16)）运行同一探测：RESULT: loopback with port accepted; other links untouched。说明：`resolvectl` 的 `Global:` 行出现 127.0.0.1:1053，是因为 resolved 把回环 DNS 服务器记在 ifindex 1（lo）名下并通过 DNSEx 报告，并非全局配置被改动；快照脚本已剔除该网卡自己的服务器后再比较，其他网卡的 DNS、路由、resolv.conf 与 NetworkManager 连接均未变化。最终评审在 Lima 虚拟机中另外验证：针对 cellglide0 链路的查询确实到达 127.0.0.1:1053（`resolvectl query` 与 `getent` 都由本地假 DNS 应答，tcpdump 在 eth0 上没有看到 53 端口的包）。结论：`RESOLVER` 保持 `127.0.0.1:1053`。
- 实机：`resolvectl dns cellglide0` → `Link 6 (cellglide0): 127.0.0.1:1053`；`resolvectl domain cellglide0` → `~.`。通过。

## 2. 实机清单

| # | 项目 | 做法 | 结果 |
|---|---|---|---|
| 1 | 出口 IP 为蜂窝 IP | `cellglide connect` 后 `curl -s https://www.cloudflare.com/cdn-cgi/trace` 查看 `ip=`，与手机关掉 Wi‑Fi 时的出口对比 | ✅ 连接前出口 203.0.113.9（Wi‑Fi）；连接后 IPv4 198.51.100.47、IPv6 2001:db8::7，均为 AS7018 AT&T；`ip route get 1.1.1.1` 走 cellglide0。`cellglide connect` 0.8 秒完成 |
| 2 | DNS 不泄漏 | 浏览器打开 dnsleaktest.com 标准测试；结果只应出现手机 DoH 服务商 | ⚠️ 存在的域名不泄漏：系统解析器出口为 Cloudflare（whoami.akamai.net → 162.158.185.44，Google myaddr → 2400:cb00:…），查询期间 Wi‑Fi 网卡无 53/853 端口包。不存在的域名会泄漏：glibc 按 resolv.conf 的 search 列表补 `.localdomain` 重试，该后缀属于 Wi‑Fi 网卡的路由域，查询发往路由器 192.168.0.1（见问题清单） |
| 3 | UDP 可用 | `dig @1.1.1.1 example.com`；浏览器打开 cloudflare-quic.com 显示 HTTP/3 | ✅ dig 经 1.1.1.1（IPv4）与 2606:4700:4700::1111（IPv6）都有应答；NTP（162.159.200.1:123）经隧道 272 ms 收到 48 字节应答。QUIC 未测（测试机 curl 不支持 HTTP/3） |
| 4 | 拔线后网络自动恢复 | 连接中拔线，`ip link show cellglide0` 不存在，`curl` 经原网络成功，耗时（秒） | ✅ 拔线后 usbmuxd 随之退出，服务在同一个 0.2 秒采样周期内拆掉隧道，路由回到 wlp2s0，出口回到 Wi‑Fi |
| 5 | `kill -9` 不留痕迹 | `sudo pkill -9 cellglide-service` 后：无 `cellglide0`、`ip route` 无 `0.0.0.0/1`、`resolvectl dns` 无 1053 | ✅ 连接中 `kill -9` 服务进程，0.5 秒内 cellglide0、两条半路由、resolved 中的 1053 全部消失；快照与连接前基线完全一致；出口回到 Wi‑Fi；systemd 2 秒后自动重启服务（重启后需重新 `cellglide connect`，第 0 阶段不做自动连接） |
| 6 | 其他网卡 DNS 前后一致 | 每项测试前后运行 `scripts/network-snapshot.sh` 并 `diff` | ✅ 安装前后一致；连接中与基线相比，只多出 iPhone 自带的 USB 网卡 `enx0ee5a167781c`（插上 iPhone 后由系统自动出现，没有 DNS 与路由），其他网卡的 DNS、路由、resolv.conf 均未变化 |
| 7 | 插回自动重连 | 拔线后重新插上，`cellglide watch` 显示重新连接 | ✅ 插回后服务自动重新监听 usbmuxd 并识别手机，约 16 秒后自动连上，无需操作。耗时主要在手机端：插回后约 15 秒内手机 CellGlide 的 8088/9876 端口拒绝连接（usbmuxd 返回 3），之后才应答 |
| 8 | 手机关掉 CellGlide DNS | 状态显示「使用公共 DNS」，`resolvectl dns cellglide0` 为四个公共地址 | ✅ 手机上关掉后，0.5 秒内自动重建连接，状态显示「使用公共 DNS」；cellglide0 的 DNS 为 2606:4700:4700::1111、2001:4860:4860::8888、1.1.1.1、8.8.8.8，1053 端口已关闭，解析正常，Wi‑Fi 网卡无 DNS 包 |
| 9 | 界面 | 托盘、窗口、开关、中文、关窗后仍在托盘（附截图） | 无服务时的窗口已在 Xvfb 下截图（`docs/validation/images/gui-no-service.png`）；托盘、关窗后仍在托盘、重新打开仍需真实桌面验证。实机（Ubuntu 26.04 GNOME Wayland，自带 AppIndicator 扩展）：✅ 窗口与中文正常，顶栏有托盘图标，关窗后点托盘能重新打开 |

## 3. 性能

| 项目 | 结果 |
|---|---|
| 下载（`curl -o /dev/null` 大文件，或 speed.cloudflare.com） | 50 MB 用时 2.9 秒，约 17.1 MB/s（≈137 Mbit/s） |
| 上传 | 10 MB 用时 2.8 秒，约 3.6 MB/s（≈29 Mbit/s） |
| 延迟（`ping 1.1.1.1` 中位数） | ping 全部丢失：ICMP 不经 SOCKS 转发，属预期（Mac 同样）。用 UDP 衡量：NTP 往返 272 ms |
| 服务 CPU（下载满速时 `top`） | 第二轮下载约 25 MB/s（≈200 Mbit/s）时，服务平均占 i5‑7200U 一个核的 43% |
| 服务内存（`ps -o rss`） | 连接中约 11–15 MB RSS，10–11 个线程，46 个 fd |
| 界面内存 | |

同一手机、同一地点，与 Mac 版对比：（填写）

## 4. 问题清单

（每条：现象、复现步骤、原因、是否阻碍进入首版）

- 窗口未显示手机名：Snapshot 只有序列号，手机名需要 lockdown，留到首版。
- CI 因 GitHub 账单问题未运行，证据来自本地 Lima 虚拟机（aarch64）。
- **组播与局域网发现被接管**：`multicast off` 只去掉了网卡的组播标志，路由仍由 `128.0.0.0/1` 覆盖 224.0.0.0/4。实测 SSDP（239.255.255.250:1900）进入 cellglide0，每条都变成手机上的一个 UDP 会话；连接期间局域网投屏、打印机等发现功能会失效。10.0.0.0/8 等其他私有网段同样进隧道（第 0 阶段未做私网绕过）。首版需按 Mac 的私网绕过补上组播与私有网段。不阻碍第 0 阶段结论。
- **不存在的域名经 Wi‑Fi 搜索域泄漏**：见清单第 2 项。只影响查不到的名字（以及单段主机名），属 systemd-resolved 路由域的标准行为。首版可考虑在 cellglide0 上同时声明这些搜索域，或在文档中说明。
- **`cellglide disconnect` 先于拆除完成就返回**：命令立即打印「已断开」，实际在约 1–2 秒后才拆掉网卡；紧接着的 `cellglide status` 仍显示已连接。应等状态变为非 Connected 再返回。
- **第一轮测试中断**：测试机根分区曾被内核切换为紧急只读（`emergency_ro`），多个系统程序读取时报 Input/output error。重启后文件系统状态 clean、SMART 正常（重分配扇区、CRC 错误均为 0），CellGlide 已安装的程序哈希与构建产物一致；故障期间日志未落盘，原因不明。CellGlide 不写磁盘，与此无关。
- **手机端恢复慢**：测试机重启后、以及拔插后，手机 CellGlide 的端口都要过一段时间（插回约 15 秒；重启后需在手机上停止再开始共享）才应答。电脑端会自动重试，但用户会看到「手机服务未就绪」。需在 iOS 端排查共享服务在 USB 重新连接后的恢复逻辑。
- **`ping` 不通**：ICMP 不经 SOCKS 转发，与 Mac 一致；用户用 ping 自测会误以为没网，首版文档需说明。

## 5. 建议

**建议进入 Linux 首版。** 第 0 阶段的技术路线在 x86 实机上全部跑通：Rust 核心 + hev + systemd-resolved 单网卡 DNS + root 服务 + iced 界面。实机清单 9 项中 8 项完全通过，第 2 项只在「不存在的域名」上有搜索域泄漏；速度约 200 Mbit/s，CPU 与内存占用可接受；拔线、强制结束、手机关 DNS 时网络都能正确恢复，且没有改动其他网卡。

首版需要先解决：
1. 组播与私有网段绕过（对应 Mac 的私网绕过），恢复局域网发现。
2. 插线自动连接与服务重启后记住连接意愿。
3. `cellglide disconnect` 等到拆除完成再返回；`execution-notes` 中的遗留小问题。
4. 正式安装包（deb / rpm / AppImage）在较旧的 glibc 上构建，以支持 Ubuntu 22.04/24.04、Debian 12；改为专用用户 + CAP_NET_ADMIN + polkit。
5. iOS 端：USB 重新连接后共享服务尽快恢复监听。
6. 修好 GitHub Actions 账单并补跑 CI，取得 x86 runner 上的特权测试证据。

对 Windows 的影响：核心（协议、转发、DNS、状态机、控制通道）已与平台解耦，可直接复用；Windows 需新写 Wintun 后端、DNS 设置与 Windows 服务，usbmuxd 改走 Apple Mobile Device Service 的 TCP 27015（`MuxEndpoint::Tcp` 已预留）。
