# Windows 1.0 W1 验证记录

机器：Windows 11 专业版 23H2 (22631) x64，管理员账户（提权不弹窗）。日期：2026-10-30。

## 开发用安装脚本（Task 14 Step 1）

脚本：`scripts\install-dev.ps1`，需要管理员 PowerShell：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 [-Release]
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 -Uninstall [-Purge]
```

### 安装做了什么

1. `cargo build [--release] -p cg-service -p cg-cli`，`fetch-wintun.ps1 -Dest target\<profile>`。
2. 停止已有的 CellGlide 服务，等 `cellglide0` 消失（最多 10 秒，超时则失败）。
3. 复制 `cellglide-service.exe`、`cellglide.exe`、`wintun.dll`、`wintun-LICENSE.txt` 到 `C:\Program Files\CellGlide\`。
4. 新建服务（`New-Service`，路径带引号，自动启动，LocalSystem）；服务已存在时用 `Win32_Service.Change` 更新路径（不用 `sc.exe config`：Windows PowerShell 5.1 传参时会丢掉 `binPath=` 里的引号，留下带空格的无引号路径）。
5. `sc.exe failure CellGlide reset= 86400 actions= restart/2000/restart/5000/restart/30000`；`sc.exe sidtype CellGlide unrestricted`。
6. 不设 `sc.exe privs`（保持默认，最小集合在 Step 3 测）。
7. 建 `C:\ProgramData\CellGlide\{health,logs}`：先把所有者设为 Administrators 并 `/reset`（防止普通用户预先建好的目录残留权限），根目录 `/inheritance:r`，SYSTEM 和 Administrators 完全控制，`NT SERVICE\CellGlide` 修改；`health\` 另给 Users 读取；`logs\` 只有继承来的三项（Users 无任何权限）。
8. `Start-Service CellGlide`，等到 Running。

### 卸载做了什么（尽力而为，可重复运行）

停服务 → 等 `cellglide0` 消失（≤10 秒）→ `sc.exe delete CellGlide` → 删 `C:\Program Files\CellGlide\` → 删 `Tcpip` / `Tcpip6` 下 `Interfaces\{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}` → 如有 non-present 的 `SWD\Wintun\{E490EEF2-...}` 设备（崩溃留下的 CM_PROB_PHANTOM），`pnputil /remove-device` 删除 → 保留 `C:\ProgramData\CellGlide\`（`-Purge` 时删除）。某一步失败只警告，最后退出码 1。

### 结果

安装后：

- `Get-Service CellGlide`：Running，Automatic；`sc.exe qc`：`BINARY_PATH_NAME : "C:\Program Files\CellGlide\cellglide-service.exe"`，`SERVICE_START_NAME : LocalSystem`；`qfailure` 三次重启 2/5/30 秒，重置 86400 秒；`qsidtype`：UNRESTRICTED；`qprivs`：空（默认）。
- `cellglide.exe status`：`No iPhone connected`（没插手机，符合预期），退出码 0。
- `cellglide.exe doctor`：6 项检查，5 项通过，"No iPhone is plugged in" 未通过（没插手机），退出码 1。
- 服务日志：`C:\ProgramData\CellGlide\logs\service.log.2026-10-30`，内容 `cellglide-service ready on \\.\pipe\cellglide-control`。

ACL（icacls）：

```
C:\ProgramData\CellGlide NT SERVICE\CellGlide:(OI)(CI)(M)
                         BUILTIN\Administrators:(OI)(CI)(F)
                         NT AUTHORITY\SYSTEM:(OI)(CI)(F)
C:\ProgramData\CellGlide\health BUILTIN\Users:(OI)(CI)(RX)
                                NT SERVICE\CellGlide:(I)(OI)(CI)(M)
                                BUILTIN\Administrators:(I)(OI)(CI)(F)
                                NT AUTHORITY\SYSTEM:(I)(OI)(CI)(F)
C:\ProgramData\CellGlide\logs NT SERVICE\CellGlide:(I)(OI)(CI)(M)
                              BUILTIN\Administrators:(I)(OI)(CI)(F)
                              NT AUTHORITY\SYSTEM:(I)(OI)(CI)(F)
```

轮次与快照（`scripts\network-snapshot.ps1`，`cellglide0` 及其 GUID 不参与比较）：

| 轮次 | 操作 | 结果 |
| --- | --- | --- |
| A1 | 安装（debug）→ 卸载 | 两次都成功；与基线 A 比较有差异，见下 |
| A2 | 安装 → 再次安装（更新路径）→ 人为制造 phantom 设备 → 卸载 | 成功；phantom 设备被删除 |
| A3 | 安装（`-Release`）→ `-Uninstall -Purge` → 干净机器上再 `-Uninstall` | 都成功（退出码 0），数据目录已删除 |
| B | 新基线 B → 安装 → 卸载 → 安装 → `-Uninstall -Purge` | 安装后、第一次卸载后、最后卸载后与基线 B 比较都是 `identical` |

A 轮的差异都来自局域网，与 CellGlide 无关（服务没有创建网卡，没插手机）：

- Wi-Fi 上新增 `fd55:ab04:2c04:48cc::/64` 及两个地址：来源 RouterAdvertisement，生命期约 29 分钟，是局域网里的路由通告。
- Wi-Fi 的 `Tcpip6\...\Dhcpv6InterfaceOptions` 变化：把 4 字节时间戳屏蔽后内容完全相同；记录里 T1 = 0x546 = 1350 秒，上次续租 10:14:00Z + 1350 秒 = 10:36:30Z，正是这次的新时间戳，即 DHCPv6 按 T1 定时续租。快照脚本的易变值正则没有覆盖这个值。

`scripts\tests\test-network-snapshot.ps1`：`all passed`。

最终状态：服务、程序目录、数据目录、`cellglide0`、phantom 设备、Tcpip 接口键都不存在。

其他观察：服务启动时 `cleanup_leftovers` 清理我们 GUID 的 DNS，会重新建出 `Tcpip\...\Interfaces\{E490EEF2-...}` 键（只属于 cellglide0，快照不比较），卸载时删除。

## 实机端到端（Task 14 Step 2）

待测（需要接上 iPhone）。

## 最小特权集合（Task 14 Step 3）

待测。
