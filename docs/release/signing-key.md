# 软件包签名密钥

用途：给 apt 仓库的 `InRelease`/`Release.gpg` 和 rpm 包、rpm 仓库的 `repomd.xml` 签名。

| 项目 | 值 |
|---|---|
| 用户 ID | CellGlide Packages <packages@cellglide.com> |
| 指纹 | `71082C14C8979B6C13E1B1D728A16C8C0A0D2A98` |
| 算法 | RSA 4096，仅签名（兼容 Ubuntu 22.04 的 apt 与各版本 rpm） |
| 创建 | 2026-10-15 |
| 到期 | 2031-10-14 |

## 存放

- 私钥只在发布者 Mac 的 GnuPG（`~/.gnupg`）里，另有一份离线备份。不放在测试机、构建机或 CI 上。
- 吊销证书由 GnuPG 自动生成：`~/.gnupg/openpgp-revocs.d/71082C14C8979B6C13E1B1D728A16C8C0A0D2A98.rev`，与私钥备份分开保存。
- 公钥随包分发：`packaging/keys/cellglide-archive-keyring.gpg`（apt，二进制）、`packaging/keys/RPM-GPG-KEY-cellglide`（rpm，ASCII）。

## 续期

到期前（例如提前三个月）延长有效期，然后重新导出公钥并发一个新版本，让已安装的用户拿到新的到期时间：

```bash
gpg --quick-set-expire 71082C14C8979B6C13E1B1D728A16C8C0A0D2A98 2y
gpg --export 71082C14C8979B6C13E1B1D728A16C8C0A0D2A98 > packaging/keys/cellglide-archive-keyring.gpg
gpg --armor --export 71082C14C8979B6C13E1B1D728A16C8C0A0D2A98 > packaging/keys/RPM-GPG-KEY-cellglide
```

## 吊销

私钥泄露时：

```bash
gpg --import ~/.gnupg/openpgp-revocs.d/71082C14C8979B6C13E1B1D728A16C8C0A0D2A98.rev
```

（导入前先删掉 `.rev` 文件里 `:-----BEGIN PGP PUBLIC KEY BLOCK-----` 这一行开头的冒号，GnuPG 加它是为了防止误导入。没有该文件时用 `gpg --gen-revoke 71082C14C8979B6C13E1B1D728A16C8C0A0D2A98` 生成。）

## 更换密钥

换钥匙需要先发一个同时信任新旧公钥的版本：包里的 keyring 同时含新旧两把公钥，用旧钥匙签名发布；等用户普遍升级后，再改用新钥匙签名，下一个版本去掉旧公钥。直接换钥匙会让已安装的用户 `apt update`/`dnf upgrade` 报签名错误，只能手动重新导入。
