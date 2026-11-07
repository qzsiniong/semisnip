# 安装

## 前置条件

- macOS（当前仅支持 macOS）
- espanso 2.4+，可执行文件在 `PATH`、`/opt/homebrew/bin` 或 `/usr/local/bin` 下（可用 `SNIPSHOT_ESPANSO_BIN` 覆盖）
- Rust 1.85+（edition 2024），仅从源码构建时需要

## 一条命令

```bash
git clone <repo> && cd snipshot
./scripts/bundle-macos.sh --install
```

脚本按顺序做这些事：

| 步骤 | 说明 |
|---|---|
| `cargo build --release` | 顺带由 `build.rs` 生成 `AppIcon.icns` |
| 组装 `Snipshot.app` | 二进制 + `Info.plist` + 图标，产出放在 `target/` |
| ad-hoc 签名 | `codesign --sign -`，够本机自用 |
| 拷到 `/Applications` | `--install` 才会执行 |
| 注册 LaunchAgent | `~/Library/LaunchAgents/com.snipshot.daemon.plist` |
| 建 CLI 软链 | `/usr/local/bin/snipshot → .app` 内的二进制 |
| 写入 espanso 触发定义 | `$ESPANSO_CONFIG/match/snipshot.yml` |
| 装 shell 补全 | 失败不中断安装，可稍后 `snipshot completion` |

不带 `--install` 时只产出 `target/Snipshot.app`，不碰系统里的任何东西。

## 之后必须手动做两步

系统不允许脚本代劳：

1. **系统设置 → 隐私与安全性 → 辅助功能** → 勾选 `Snipshot`
2. `espanso restart`

然后体检：

```bash
snipshot doctor
```

`doctor` 逐项检查 espanso、配置目录、片段索引、daemon、辅助功能，最后打印当前生效的配置。看到 `全部正常。` 就可以用了；如果显示「可用，但处于降级模式」，说明辅助功能没授权，`;;` 会绕道 espanso 的 shell 变量，能用但多一次进程启动、也没有零残留。

## 为什么要打成 `.app`

裸二进制做不了这两件事：

- **TCC 授权以签名为单位**，裸二进制在辅助功能列表里会变成一个路径，重建即失效
- **`LSUIElement` 只写在 `Info.plist` 里**，没有 bundle 就没有这个键，进程会以 Regular 身份进 Dock

所以 `bundle-macos.sh` 不是「方便」，是必需品。

## `zsh: command not found: snipshot`

软链没建上。二进制本体在 `.app` 里，`/Applications` 不在 `PATH` 上，手动补一条：

```bash
sudo ln -sf /Applications/Snipshot.app/Contents/MacOS/snipshot /usr/local/bin/snipshot
hash -r
```

不想动 `/usr/local/bin`，也可以：

```bash
alias snipshot=/Applications/Snipshot.app/Contents/MacOS/snipshot
```

## 关于 ad-hoc 签名：每次重建都要重新授权

`codesign --sign -` 是临时签名，**每次 rebuild 的 cdhash 都会变**。TCC 按签名记账，旧授权对不上新二进制就被判失效——症状是 `doctor` 里「当前进程有授权，但 daemon 没有」。

处理：系统设置 → 隐私与安全性 → 辅助功能，把 `Snipshot` 删掉再重新添加（只取消勾选再勾上有时匹配不到新二进制）。

要根治，得放一张自签名代码签名证书进钥匙串，然后把脚本里的 `--sign -` 换成证书名。这样 TCC 的 designated requirement 会钉在「identifier + 证书」上，rebuild 不再掉授权。本机目前没有可用签名身份（`security find-identity -v -p codesigning` 为空），所以还是 ad-hoc。

## 分发

ad-hoc 签名只够本机自用。要给别人用，把 `scripts/bundle-macos.sh` 里的 `codesign --sign -` 换成 Developer ID，并补上 notarization，否则对方首次打开会被 Gatekeeper 拦下。

## 卸载

```bash
snipshot uninstall                   # 演练，列出每一项
snipshot uninstall --yes             # 真删，保留配置与日志
snipshot uninstall --yes --purge     # 连配置、使用频率、日志一起删
```

细节见 [命令行 · uninstall](cli.md#uninstall)。
