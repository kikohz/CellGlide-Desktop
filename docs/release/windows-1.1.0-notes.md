# CellGlide for Windows 1.1.0 发布说明（草稿）

发布时把下面三种语言的条目放进 CellProxy `Website/changelog.json` 的一条新记录：`"platform": "windows"`、`"version": "1.1.0"`、`"date"` 为发布当天。

待定：iPhone 上需要的 CellGlide 版本号。iOS 端 `windows-hotspot` 分支（`hotspot-v1`，拔线后自动回到无线靠它）发版后，填进最后一条的 `<iOS 版本>`。

## zh

- 无线连接：不插线也能通过 iPhone 上网。默认关闭，在设置里打开后，插上数据线就会自动配对，也可以用验证码配对；之后点「无线连接」，电脑会开一个临时的无线网络，iPhone 加入后就连上了
- 无线连着时插上数据线，自动转到 USB；拔掉后回到无线
- 电脑睡眠唤醒后，无线连接会自动重新连上
- 无线连接期间，电脑自己连着的 Wi‑Fi 不受影响；断开后临时无线网络随即关闭
- 无线连接取决于无线网卡，需要支持 Wi‑Fi Direct；已在 Intel Wi‑Fi 6 AX201 上验证。不支持时，设置里会说明原因
- 需要 iPhone 上的 CellGlide <iOS 版本> 或更新版本
- ARM64 版仍是测试版

## en

- Wireless connection: get online through your iPhone without a cable. It is off by default; once you turn it on in Settings, plugging the cable in pairs the iPhone on its own, or you can pair with a code. After that, click Connect Wirelessly: the computer opens a temporary wireless network, and the iPhone joins it and connects
- While connected wirelessly, plugging the cable in switches to USB; unplugging it goes back to wireless
- After the computer sleeps and wakes, the wireless connection comes back on its own
- The Wi‑Fi your computer is on is not affected; the temporary wireless network closes when you disconnect
- Wireless connection depends on the wireless card, which must support Wi‑Fi Direct; tested on Intel Wi‑Fi 6 AX201. When it isn't supported, Settings says why
- Requires CellGlide <iOS version> or later on the iPhone
- The ARM64 version is still a test build

## es

- Conexión inalámbrica: conéctate a internet a través del iPhone sin cable. Viene desactivada; cuando la activas en Ajustes, al conectar el cable el iPhone se empareja solo, o puedes emparejar con un código. Después, haz clic en Conectar sin cable: el ordenador abre una red inalámbrica temporal y el iPhone se une a ella y se conecta
- Con la conexión inalámbrica activa, al conectar el cable se pasa a USB; al desconectarlo se vuelve a la conexión inalámbrica
- Cuando el ordenador sale de la suspensión, la conexión inalámbrica se recupera sola
- El Wi‑Fi al que está conectado el ordenador no se ve afectado; la red inalámbrica temporal se cierra al desconectar
- La conexión inalámbrica depende de la tarjeta inalámbrica, que debe admitir Wi‑Fi Direct; probada con Intel Wi‑Fi 6 AX201. Si no es compatible, Ajustes explica el motivo
- Requiere CellGlide <versión de iOS> o posterior en el iPhone
- La versión ARM64 sigue siendo una versión de prueba
