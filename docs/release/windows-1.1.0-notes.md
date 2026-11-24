# CellGlide for Windows 1.1.0 发布说明（草稿）

发布时把下面三种语言的条目放进 CellProxy `Website/changelog.json` 的一条新记录：`"platform": "windows"`、`"version": "1.1.0"`、`"date"` 为发布当天。

待定（发布闸门，2026-11-23 终审 I-3）：iPhone 上需要的 CellGlide 版本号。拔线后回到无线（尤其是插着线时睡眠过）靠 iOS 端 `windows-hotspot` 分支的 `hotspot-v1`，目前只在未发版的测试机上验过。iOS 发版前不打标签、不发布；发版后把版本号填进 `<iOS 版本>`，并用正式版 iOS 重跑一次「插线、睡眠、唤醒、拔线」。

## zh

- 无线连接：不插线也能通过 iPhone 上网。默认关闭，在设置里打开后，插上数据线、在 iPhone 上允许一次就会配对，也可以用验证码配对；之后点「无线连接」，电脑会开一个临时的无线网络，iPhone 加入后就连上了
- 无线连着时插上数据线，自动转到 USB；拔掉后回到无线（iPhone 可能请你点一下加入）
- 已知情况：iPhone 记住的其他 Wi‑Fi 在附近时，它可能自己换过去；这时在 iPhone 的“设置 → 无线局域网”里重新选 CellGlide 的网络
- 电脑睡眠唤醒后，无线连接会自动重新连上
- 无线连接期间，电脑自己连着的 Wi‑Fi 不受影响；断开后临时无线网络随即关闭
- 无线连接取决于无线网卡，需要支持 Wi‑Fi Direct；已在 Intel Wi‑Fi 6 AX201 上验证。不支持时，设置里会说明原因
- 需要 iPhone 上的 CellGlide <iOS 版本> 或更新版本
- ARM64 版仍是测试版

## en

- Wireless connection: get online through your iPhone without a cable. It is off by default; once you turn it on in Settings, plugging the cable in pairs the iPhone once you allow it there, or you can pair with a code. After that, click Connect Wirelessly: the computer opens a temporary wireless network, and the iPhone joins it and connects
- While connected wirelessly, plugging the cable in switches to USB; unplugging it goes back to wireless (the iPhone may ask you to tap Join)
- Known issue: when another Wi‑Fi network the iPhone remembers is nearby, it may switch to it; if so, pick the CellGlide network again in Settings → Wi‑Fi on the iPhone
- After the computer sleeps and wakes, the wireless connection comes back on its own
- The Wi‑Fi your computer is on is not affected; the temporary wireless network closes when you disconnect
- Wireless connection depends on the wireless card, which must support Wi‑Fi Direct; tested on Intel Wi‑Fi 6 AX201. When it isn't supported, Settings says why
- Requires CellGlide <iOS version> or later on the iPhone
- The ARM64 version is still a test build

## es

- Conexión inalámbrica: conéctate a internet a través del iPhone sin cable. Viene desactivada; cuando la activas en Ajustes, al conectar el cable el iPhone se empareja cuando lo permites en él, o puedes emparejar con un código. Después, haz clic en Conectar sin cable: el ordenador abre una red inalámbrica temporal y el iPhone se une a ella y se conecta
- Con la conexión inalámbrica activa, al conectar el cable se pasa a USB; al desconectarlo se vuelve a la conexión inalámbrica (puede que el iPhone te pida tocar Conectar)
- Problema conocido: si hay cerca otra red Wi‑Fi que el iPhone recuerda, puede cambiarse a ella; en ese caso, vuelve a elegir la red de CellGlide en Ajustes → Wi‑Fi del iPhone
- Cuando el ordenador sale de la suspensión, la conexión inalámbrica se recupera sola
- El Wi‑Fi al que está conectado el ordenador no se ve afectado; la red inalámbrica temporal se cierra al desconectar
- La conexión inalámbrica depende de la tarjeta inalámbrica, que debe admitir Wi‑Fi Direct; probada con Intel Wi‑Fi 6 AX201. Si no es compatible, Ajustes explica el motivo
- Requiere CellGlide <versión de iOS> o posterior en el iPhone
- La versión ARM64 sigue siendo una versión de prueba
