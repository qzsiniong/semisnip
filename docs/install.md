# 安装

## 两条路

- **用现成的包**：到 [Releases](https://github.com/qzsiniong/semisnip/releases) 下 `Semisnip-<版本>-universal.dmg`（arm64 + x86_64 通用），拖进 `Applications`。包**未签名/未公证**，首次打开先 `xattr -cr /Applications/Semisnip.app`，或右键 →「打开」→ 再点一次「打开」。装完仍要做下面那两步手动授权。
- **从源码装**：见下面的「一条命令」，会顺带装好 LaunchAgent 与 CLI 软链。

## 前置条件

- macOS（当前仅支持 macOS）
- espanso 2.4+，可执行文件在 `PATH`、`/opt/homebrew/bin` 或 `/usr/local/bin` 下（可用 `SEMISNIP_ESPANSO_BIN` 覆盖）
- Rust 1.85+（edition 2024），仅从源码构建时需要

## 一条命令

```bash
git clone <repo> && cd semisnip
./scripts/bundle-macos.sh --install
```

脚本按顺序做这些事：

| 步骤 | 说明 |
|---|---|
| `cargo build --release` | 顺带由 `build.rs` 生成 `AppIcon.icns` |
| 组装 `Semisnip.app` | 二进制 + `Info.plist` + 图标，产出放在 `target/` |
| 注入版本号 | 从 `Cargo.toml` 写进 `Info.plist` 的 `CFBundleShortVersionString`（在签名之前） |
| ad-hoc 签名 | `codesign --sign -`，够本机自用 |
| 拷到 `/Applications` | `--install` 才会执行 |
| 注册 LaunchAgent | `~/Library/LaunchAgents/com.semisnip.daemon.plist` |
| 建 CLI 软链 | `/usr/local/bin/semisnip → .app` 内的二进制 |
| 写入 espanso 触发定义 | `$ESPANSO_CONFIG/match/semisnip.yml` |
| 装 shell 补全 | 失败不中断安装，可稍后 `semisnip completion` |

不带 `--install` 时只产出 `target/Semisnip.app`，不碰系统里的任何东西。要出通用二进制或 dmg/zip，加 `--universal` / `--dist`，见 [发版](release.md)。

## 双击图标 = 启动

装上之后 `Semisnip.app` 可以当启动开关用（Finder 里双击，或 `open -a Semisnip`）：

| 当前状态 | 双击的结果 |
|---|---|
| 没有 daemon 在跑 | 自己起一个，菜单栏出现图标；首次运行还会弹引导页 |
| 已有 daemon 在跑 | 把片段面板叫出来（双击总得有看得见的反应） |

命令行的行为没变：终端里裸敲 `semisnip` 仍然只打印帮助——一个会抢键盘的常驻进程不该是一条裸命令的默认后果。

双击启动**不会开机自启**：那只由 `--install` 写的 LaunchAgent 负责。所以从菜单栏点过「退出 semisnip」之后，双击是把它叫回来的正路（launchd 见退出码 0 就不再拉起，见 [菜单栏](tray.md)）。

## 之后必须手动做两步

系统不允许脚本代劳：

1. **系统设置 → 隐私与安全性 → 辅助功能** → 勾选 `Semisnip`
2. `espanso restart`

**勾上就生效，不用重启 daemon。** daemon 每秒对齐一次 tap 状态、每 5 秒重试一次安装，授权一到面板自己就恢复正常；如果列表里本来就是旧条目（重装换了签名，勾着也不管用），先把它删掉再重新添加，然后点面板上的「重新检查」。

没授权时 `;;` 仍然能打开面板，但里面收不到按键——所以面板会整块换成「需要授权」页，而不是摆一列你打了字也不会变的候选。

然后体检：

```bash
semisnip doctor
```

`doctor` 逐项检查 espanso、配置目录、片段索引、daemon、辅助功能，最后打印当前生效的配置。看到 `全部正常。` 就可以用了；如果显示「可用，但处于降级模式」，说明辅助功能没授权——这时打 `;;;`（触发串多打一个末字符，见 [架构 · 两条回传协议](architecture.md#两条回传协议)）会绕道 espanso 的 shell 变量打开面板，能用但多一次进程启动，而且面板只有授权引导页（收不到键盘）。

## 为什么要打成 `.app`

裸二进制做不了这两件事：

- **TCC 授权以签名为单位**，裸二进制在辅助功能列表里会变成一个路径，重建即失效
- **`LSUIElement` 只写在 `Info.plist` 里**，没有 bundle 就没有这个键，进程会以 Regular 身份进 Dock

所以 `bundle-macos.sh` 不是「方便」，是必需品。

## `zsh: command not found: semisnip`

软链没建上。二进制本体在 `.app` 里，`/Applications` 不在 `PATH` 上，手动补一条：

```bash
sudo ln -sf /Applications/Semisnip.app/Contents/MacOS/semisnip /usr/local/bin/semisnip
hash -r
```

不想动 `/usr/local/bin`，也可以：

```bash
alias semisnip=/Applications/Semisnip.app/Contents/MacOS/semisnip
```

## 关于 ad-hoc 签名：每次重建都要重新授权

`codesign --sign -` 是临时签名，**每次 rebuild 的 cdhash 都会变**。TCC 按签名记账，旧授权对不上新二进制就被判失效——症状是 `doctor` 里「当前进程有授权，但 daemon 没有」。

处理：系统设置 → 隐私与安全性 → 辅助功能，把 `Semisnip` 删掉再重新添加（只取消勾选再勾上有时匹配不到新二进制）。

要根治，得放一张自签名代码签名证书进钥匙串，然后把脚本里的 `--sign -` 换成证书名。这样 TCC 的 designated requirement 会钉在「identifier + 证书」上，rebuild 不再掉授权。本机目前没有可用签名身份（`security find-identity -v -p codesigning` 为空），所以还是 ad-hoc。

## 分发

ad-hoc 签名只够本机自用。要给别人用，把 `scripts/bundle-macos.sh` 里的 `codesign --sign -` 换成 Developer ID，并补上 notarization，否则对方首次打开会被 Gatekeeper 拦下。

当前是通过 GitHub Release 分发**未签名**的 universal 包，靠「右键 → 打开」放行；打 tag 之后流水线做了什么、产物长什么样，见 [发版](release.md)。

## 卸载

```bash
semisnip uninstall                   # 演练，列出每一项
semisnip uninstall --yes             # 真删，保留配置与日志
semisnip uninstall --yes --purge     # 连配置、使用频率、日志一起删
```

细节见 [命令行 · uninstall](cli.md#uninstall)。
