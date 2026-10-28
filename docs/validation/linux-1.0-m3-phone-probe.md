# Linux 上经 USB 能读到的手机电池数据（M3 Task 4）

日期：2026-10-16
测试机：Ubuntu 26.04.1 x86_64；usbmuxd 由系统包提供；配对记录在 `/var/lib/lockdown`（属主 usbmux:plugdev，其他人可读）
手机：iPhone17,5（iPhone 16e），iOS 26.6，AT&T，手机 CellGlide 2.2.0，经 USB 数据线接在电脑上
程序：`cargo build --release -p cg-core --example phone_probe`（在 `cellglide-builder:22.04` 容器里编译），复制到 `/usr/local/bin/cg-phone-probe`，以服务用户运行：`sudo -u cellglide cg-phone-probe`

## 结论

- 以 `cellglide` 用户身份就能读到配对记录，建立 lockdown 会话，并用配对记录里的证书升级到 TLS。rustls 和 iOS 26.6 握手成功，Task 3 的 TLS 设置不用改。
- diagnostics relay 能打开，IORegistry 的电池字典能读到。
- 手机**解锁**与**锁屏**两种状态下结果相同，锁屏不影响读取。
- 不需要改 iOS。

## 两次输出（去掉标识）

解锁、正在共享：

```
pair record: ok
label model: iPhone17,5
session: ok
tls: on
info: model=iPhone17,5 ios=26.6 sims=com.apple.ATT_aio_NR_US 310-150 baseband=… tray=kCTSIMSupportSIMTrayAbsent
relay: ok (tls on)
battery: level=80 charging=false external_connected=true fully_charged=false watts=absent input_watts=absent minutes_to_full=absent adapter_watts=3 wireless=false health=1 cycles=1 design_capacity=3961 full_charge_capacity=4108 temperature=33.39
```

锁屏、数据线仍插着：除温度（33.29）外与上面相同。

IORegistry 实际出现的键（共 56 个）：AbsoluteCapacity, AdapterDetails, AdapterInfo, Amperage, AppleRawAdapterDetails, AppleRawBatteryVoltage, AppleRawCurrentCapacity, AppleRawExternalConnected, AppleRawMaxCapacity, AtCriticalLevel, AvgTimeToEmpty, BatteryCellDisconnectCount, BatteryData, BatteryInstalled, BatteryInvalidWakeSeconds, BestAdapterIndex, BootPathUpdated, BootVoltage, CarrierMode, ChargerConfiguration, ChargerData, CurrentCapacity, CycleCount, DeadBatteryBootData, DesignCapacity, ExternalChargeCapable, ExternalConnected, FedDetails, FullPathUpdated, FullyCharged, GasGaugeFirmwareVersion, IOGeneralInterest, IOReportLegend, IOReportLegendPublic, InductiveData, InstantAmperage, IsCharging, KioskMode, LPEMData, Location, ManufacturerData, MaxCapacity, NominalChargeCapacity, PortControllerInfo, PostChargeWaitSeconds, PostDischargeWaitSeconds, PowerTelemetryData, Serial, SkipperNEIgnoreAtCritical, Temperature, TimeRemaining, UpdateTime, UserVisiblePathUpdated, VirtualTemperature, Voltage, built-in。

## Mac 卡片各项在 Linux 上的情况

| 项目 | Linux | 说明 |
|---|---|---|
| 电量 | 读得到 | `level=80` |
| 是否接着电源 | 读得到 | `ExternalConnected` |
| 正在充电 | 读得到 | `IsCharging`。这次为 false，手机 App 却报「充电中」，见下方「注意」 |
| 充电功率 | 这次没有值 | 没在充电时 `InstantAmperage` 不大于 0，按 Mac 的规则不算功率；充电时应能算出 |
| 供电功率（适配器） | 读得到 | `adapter_watts=3`，电脑 USB 口约 2.5–3 W |
| 无线充电 | 读得到 | `wireless=false` |
| 温度 | 读得到 | 33.4 °C；按 Mac 的规则不报给手机 |
| 电池健康 | 读得到 | `health=1`（满分），由 `FullChargeCapacity / DesignCapacity` 算出，上限为 1 |
| 循环次数 | 读得到 | `cycles=1` |
| 充满还需时间 | 这次没有值 | 只在充电且功率 ≥ 0.5 W 时计算 |
| 型号 | 读得到 | `iPhone17,5`，查表得名称 |
| iOS 版本 | 读得到 | 26.6 |
| SIM 与副品牌 | 读得到 | 运营商 bundle 与 MCC/MNC |

## 注意

- 同一时刻 lockdown 报 `IsCharging=false`，手机 App 回传的帧报「充电中」。电脑 USB 口只给约 3 W，手机可能处于「接着电但没在充」或间歇充电的状态，两边取样的时机与含义不同。界面以 lockdown 读数为准显示「充电 / 接着电源但未充电」，与 Mac 相同（Mac 也用 IORegistry）。
- 读不到或这次没有值的项（充电功率、充满所需时间）按 spec §3 反目标不显示，不填 0。
