# snipshot

**espanso 的片段检索层。** 敲 `;;` 打开一个不抢焦点的小浮层，边打边过滤、右侧预览，回车把选中的 trigger 交回 espanso 求值并注入。

snipshot 不存片段、不渲染变量、不注入文本——这些仍然全部由 espanso 负责。它只替换 espanso 自带的搜索框那一层。

```
任意输入框  →  ;;  →  过滤 + 预览  →  回车  →  espanso 展开
```

## 为什么不用 espanso 自带的搜索框

| | espanso search bar | snipshot |
|---|---|---|
| 焦点 | 抢焦点，目标 App 失焦 | 非激活浮层，目标 App 全程不失焦 |
| 数据 | espanso 运行时 | 直接读 `match/**/*.yml`，文件改动即热更新 |
| 检索 | 简单子串 | 分档打分 + 使用频率 + 拼音全拼/首字母 |
| 预览 | 无 | 只读预览，`shell` / `script` 绝不执行 |
| 展开 | 自己注入 | `espanso match exec -t <trigger>`，交回 espanso |

## 前置条件

- macOS（当前仅支持 macOS）
- espanso 2.4+，且在 `PATH` 或 `/opt/homebrew/bin`、`/usr/local/bin` 下（可用 `SNIPSHOT_ESPANSO_BIN` 覆盖）
- Rust 1.85+（edition 2024）

## 安装

```bash
git clone <repo> && cd snipshot
./scripts/bundle-macos.sh --install
```

脚本会依次做：release 构建 → 组装 `Snipshot.app` → ad-hoc 签名 → 拷到 `/Applications` → 注册 LaunchAgent → 写入 `$ESPANSO_CONFIG/match/snipshot.yml` → 建 CLI 软链 `/usr/local/bin/snipshot` → 装好 shell 补全。

之后还有两步必须手动完成（系统不允许脚本代劳）：

1. **系统设置 → 隐私与安全性 → 辅助功能** → 勾选 `Snipshot`
2. `espanso restart`

最后体检：

```bash
snipshot doctor
```

四项全绿即可使用。`doctor` 会检查 espanso 可执行文件、配置目录、片段索引、daemon 存活与辅助功能授权。

> **`zsh: command not found: snipshot`** —— 说明软链没建上。二进制本体在 `.app` 里，`/Applications` 不在 PATH 上，手动补一条即可：
>
> ```bash
> sudo ln -sf /Applications/Snipshot.app/Contents/MacOS/snipshot /usr/local/bin/snipshot
> hash -r
> ```
>
> 不想动 `/usr/local/bin`，也可以先 `alias snipshot=/Applications/Snipshot.app/Contents/MacOS/snipshot`。

> ad-hoc 签名只够本机自用。要分发给别人，把 `scripts/bundle-macos.sh` 里的 `codesign --sign -` 换成 Developer ID。

## 使用

| 操作 | 效果 |
|---|---|
| `;;` | 打开面板（第二个 `;` 被吞掉，第一个被回删） |
| 直接输入 | 过滤，空格分词、全部 AND |
| `↑` / `↓` | 上下选择，循环 |
| `Enter` | 交给 espanso 展开 |
| `Esc` | 关闭，并回删触发符残留 |
| `Backspace` | 删除过滤字符 |
| `Cmd` / `Ctrl` + 任意键 | 放行给系统并关闭面板，不吞系统快捷键 |

几个细节：

- **trigger 的核心部分可省略前缀**：打 `log` 就能命中 `;;log`，打 `date` 命中 `:date`。
- **拼音可用**：标签会被索引成全拼与首字母，`rq` 能命中「日期」。
- **排序**：精确 `1000` > 前缀 `800~700` > 子串 `600` > 子序列 `300`，再叠加使用频率与最近使用时间。使用统计存在 `usage.json`，丢了只影响排序，不影响功能。
- **`$|$` 光标标记**在预览里原样保留，不会被当成变量。

## 菜单栏图标

daemon 是常驻进程，但**不进 Dock、不进 Cmd-Tab**：`Info.plist` 里 `LSUIElement=true`，二进制启动后还会再调一次 `set_activation_policy(Accessory)`——只靠 plist 不够，gpui 启动时会把策略按自己的默认（Regular）设回去。

用户能看见它的地方只有菜单栏那一个图标（模板图，深浅色自动反色）：

```
打开片段面板        ← 和 `;;` 等价，只是不用切回输入框
──────────
重建索引
──────────
打开配置…           ← 用系统默认程序打开 config.json
打开 snipshot.yml…  ← espanso 的那个降级触发定义（不存在时打开其目录）
打开日志…
──────────
退出 snipshot       ← 走 Shutdown，先关面板再退，launchd 不会把它拉回来
```

标记是「斜体 `I` + `;;`」：`I` 是光标，`;;` 是触发词，合起来就是产品那一句话。菜单栏用的也是它——同一个程序在两个地方必须是同一个记号。

图标是**生成**的，仓库里不放二进制素材：形状在 `src/mark.rs` 里是一组解析式矩形，`build.rs` 在构建期把它渲染成 PNG 和 ICNS。所以：

- 改设计只改 `src/mark.rs` 里的几个常量，`cargo build` 自动重画；
- 每次构建都从零采样（4×4 子样本算覆盖率），16px 和 1024px 用的是同一套线条，不存在「大图缩下来发糊」；
- 不需要 Pillow、也不需要 `iconutil`——ICNS 就是「魔数 + 一串 (类型, 长度, PNG)」，`build.rs` 里手拼，Linux 上 `cargo build` 也不会断。

产出都在 `packaging/macos/`（内容没变时不重写，免得刷 mtime）：

| 文件 | 用途 |
|---|---|
| `AppIcon.icns` | 10 个槽位（16…1024，含 @2x），`CFBundleIconFile` 指向它 |
| `icon-preview.png` | 512px 预览，就是上面那张图 |
| `tray-preview.png` | 菜单栏图标的深浅底色对照（放大 8 倍，用来看 15pt 下认不认得出来） |

应用图标长这样：

![应用图标](packaging/macos/icon-preview.png)

菜单栏图标是同一个标记，只是更小（15pt 高、按比例宽 20pt）：

![菜单栏图标](packaging/macos/tray-preview.png)

> **图标没出现？** 先看菜单栏最左侧、刘海右边有没有一个 `«` 双箭头。有的话说明菜单栏已经排满，macOS 把新图标折叠进溢出区了——新建的状态项永远排在右边那组的末尾，也就是最先被折叠的那个。随手退出一个菜单栏常驻 app，或按住 ⌘ 拖动图标调整位置，`I` 就出来了。这个和 snipshot 本身无关：用纯文本状态项做的最小 AppKit 探针在同样环境里也一样看不见。

## 配置

路径：`~/Library/Application Support/snipshot/config.json`（`snipshot install` 时生成）

```json
{
  "trigger": ";;",
  "trigger_timeout_ms": 800,
  "focus_mode": "nonactivating",
  "max_results": 9,
  "panel_width": 720.0,
  "panel_height": 420.0,
  "post_exec_delay_ms": 12,
  "preview_clipboard": false,
  "show_detail": true,
  "allow_paste_fallback": true
}
```

| 字段 | 默认 | 说明 |
|---|---|---|
| `trigger` | `";;"` | 只允许 ASCII，会被编译成物理键码序列 |
| `trigger_timeout_ms` | `800` | 两个触发键之间的最大间隔 |
| `focus_mode` | `"nonactivating"` | 预留项；`activating` 分支尚未实现 |
| `max_results` | `9` | 候选条数上限 |
| `panel_width` / `panel_height` | `720` / `420` | 面板尺寸（最小 360×200） |
| `post_exec_delay_ms` | `12` | 回删到 `match exec` 之间的等待 |
| `preview_clipboard` | `false` | 详情区是否显示剪贴板全文；`false` 时只显示字符数 |
| `show_detail` | `true` | 是否显示右侧详情区 |
| `allow_paste_fallback` | `true` | `match exec` 失败时用「安全渲染 + 剪贴板粘贴」兜底 |

snipshot 自己的文件只有这几个，卸载不会碰到任何片段数据：

```
~/Library/Application Support/snipshot/config.json   偏好
~/Library/Application Support/snipshot/usage.json    使用频率
~/Library/Application Support/snipshot/snipshot.log  日志
~/Library/Caches/snipshot/snipshot.sock              daemon socket
```

## 命令行

`snipshot` 不带参数时**只打印帮助**——daemon 是常驻进程，会抢键盘、要授权，不该是一条裸命令的默认后果。daemon 交给 LaunchAgent 管。

```
snipshot                            打印帮助（不会启动任何东西）
snipshot daemon                     前台启动 daemon（调试用）
snipshot trigger [--origin O]       请求打开面板（origin: tap | espanso | manual）
snipshot reload                     重建索引
snipshot install                    写入 espanso 触发定义并打印后续步骤
snipshot match [子命令]              看/改写进 espanso 的 snipshot.yml
snipshot uninstall [--yes]          卸载（默认只演练，--purge 连配置一起删）
snipshot completion [子命令]         生成/安装 shell 补全
snipshot config [子命令]             配置的查看与修改
snipshot log [选项]                  看日志
snipshot doctor                     体检
snipshot type <text>                调试：合成字符输入
snipshot key <esc|enter|up|down>    调试：合成命名按键
```

命令树用 clap 定义，`--help` / `-V` / 拼错时的「你是不是想输 xxx」都是自动的；`type` / `key` 是隐藏的调试命令，不出现在帮助里。

`trigger` 被 espanso 的 shell 变量同步调用，因此**成功时 stdout 必须为空**——espanso 会把 stdout 原样注入文档，多一个字节就是污染。所有错误都走 stderr。

`type` / `key` 会向系统合成真实键盘事件，仅用于没有辅助功能权限时的冒烟测试。

### `match`

snipshot 只往 espanso 的 `match/` 里写**一个**文件：`snipshot.yml`。同目录下其它的 YAML 都是你的片段，任何命令都不碰。

```bash
snipshot match                       # 打印内容（默认动作，原样输出，方便管道）
snipshot match path                  # 打印路径
snipshot match edit                  # $EDITOR 打开，退出后自动校验
snipshot match open                  # 用系统默认程序打开
snipshot match open --reveal         # 在访达里选中它（不打开）
snipshot match regenerate            # 按当前 exe 路径与 trigger 重写（= install 的写文件那一步）
```

`edit` 退出后只查「能不能被 espanso 用起来」这一层，不试图替代 espanso 的解析器：

- YAML 能否解析、`matches` 是否非空（硬错误，退出码 1）
- `trigger` 是否与 `config.trigger` 一致——tap 路径按配置里的 trigger 吞键，两边不一致时降级触发永远对不上
- shell 变量的 `cmd` 有没有把 stdout 重定向到 `/dev/null`——漏了它，命令输出会被原样注入用户文档

后两条是警告，不拦着你保存。这个文件是**生成物**：`install` / `regenerate` 都会整份重写，手改只适合临时调试；要长期改触发串请用 `snipshot config set trigger`。

### `completion`

子命令有十来个，靠记不现实，所以补全被当成主路径的一部分：

```bash
snipshot completion                 # 按 $SHELL 装好（写脚本 + 接进启动文件）
snipshot completion --dry-run       # 只说明将要改什么
snipshot completion zsh             # 只把脚本打到 stdout，给 eval 用
snipshot completion install --shell fish
snipshot completion uninstall       # 摘干净：删脚本 + 移除启动文件里的接管块
```

装的时候会在启动文件里写一段**带标记**的块，`uninstall` 靠标记原样摘除，不猜哪几行是 snipshot 写的：

```zsh
# >>> snipshot completion >>>
if (( $+functions[compdef] )) && [ -r ".../snipshot/completions/snipshot.zsh" ]; then
  source ".../snipshot/completions/snipshot.zsh"
fi
# <<< snipshot completion <<<
```

- zsh → `~/.zshrc`（`compdef` 还没就绪时静默跳过，不会每次开终端报错）
- bash → `~/.bash_profile`（存在则用它，其次 `~/.bashrc`）
- fish → `~/.config/fish/completions/snipshot.fish`，fish 自动加载，**不改**任何 rc
- powershell → `$PROFILE`（`~/.config/powershell/Microsoft.PowerShell_profile.ps1`）
- elvish 没有约定俗成的自动加载点，只支持 `snipshot completion elvish` 自己 eval

重复执行是幂等的：块就地替换，不会越装越长。

### `config`

```bash
snipshot config                      # 当前值，与默认值不同的项标 *
snipshot config get trigger          # 取值（字符串不带引号，方便塞进脚本）
snipshot config set max_results 12   # 改一个键，带类型与范围校验
snipshot config unset max_results    # 恢复默认
snipshot config reset                # 全部恢复默认
snipshot config keys                 # 列出所有键与含义
snipshot config edit                 # $EDITOR 打开，退出时自动校验 JSON
snipshot config path                 # 配置文件路径
```

`set` 会先按默认值的类型解析（`true/false`、整数、小数、字符串），再做语义校验：`trigger` 必须能映射成物理键码（中文会直接报错），`max_results` 限 1–100，`panel_width/height` 限 200–2000。写入时先落临时文件再改名，daemon 不会读到半截 JSON。

配置在 daemon 启动时读一次，改完它会打印 `launchctl kickstart` 的完整命令。

### `log`

```bash
snipshot log                         # 最近 40 行（默认 40，-n 改）
snipshot log -n 200                  # 最近 200 行
snipshot log -f                      # 跟随，Ctrl-C 退出
snipshot log --grep 触发              # 只看包含关键词的行
snipshot log --level warn            # 只看 warn 及以上（info/warn/error，大小写不敏感）
snipshot log path                    # 日志路径
snipshot log clear                   # 清空
```

因为 daemon 以 `.app` 启动时没有终端，日志文件是唯一的回溯手段。`--level` 读的是 `env_logger` 那行 `[时间 LEVEL target]` 里的级别；没有前缀的续行（栈回溯）一律放行，不会漏掉上下文。

### `uninstall`

默认**只演练**，把每一项和路径都列出来，加 `--yes` 才真删：

```bash
snipshot uninstall                   # 演练
snipshot uninstall --yes             # 真删（保留配置与日志）
snipshot uninstall --yes --purge     # 连配置、使用频率、日志一起删
snipshot uninstall --yes --keep-app  # 只拆 LaunchAgent 与 espanso 定义，留 app
```

顺序是先 `launchctl bootout` 再删 app——launchd 的 `ProgramArguments` 指向 app 内部，反过来会触发一次失败重启。**任何情况下都不碰 espanso 的片段 YAML**，只删自己生成的那一个 `match/snipshot.yml`。完成后会提醒你 `espanso restart`。

## 架构

```
snipshot trigger ──UDS NDJSON──┐
CGEventTap ────────────────────┴─→ daemon(AppState) ─→ index(ArcSwap<Snapshot>)
                                                       ├→ search(rank)
                                                       ├→ preview(safe)
                                                       └→ handoff(match exec)
```

| 模块 | 文件 | 职责与边界 |
|---|---|---|
| 触发 | `trigger/tap.rs`、`trigger/machine.rs`、`trigger/keymap.rs` | 只认物理键码，不知道片段存在；`;;` 的第二个键被吞、第一个键记为待回删 |
| 通信 | `ipc/proto.rs`、`ipc/server.rs`、`ipc/client.rs` | UDS + NDJSON，带版本号；单实例，daemon 不在时由 CLI 自愈拉起 |
| 数据 | `index/loader.rs`、`index/service.rs` | 唯一读 YAML 的地方，产出 `Arc<Snapshot>`；后台重建，UI 永远只读 |
| 检索 | `search/score.rs`、`search/query.rs`、`search/pinyin.rs` | 纯函数、零 IO、确定性排序 |
| 预览 | `preview/vars.rs` | 只吃 `&Snippet`，**绝不执行 shell/script** |
| 回传 | `handoff/mod.rs`、`handoff/inject.rs` | 只认 `Hit` 与 trigger 字符串，不关心 YAML 结构 |
| 界面 | `ui/window.rs`、`ui/view.rs` | `WindowKind::PopUp`（NSPanel + NonactivatingPanel），只渲染，不处理键盘 |
| 菜单栏 | `tray.rs` | NSStatusItem + 原生菜单；模板图（深浅色自动反色），菜单项经 GPUI action 回到 scheduler 的事件通道 |
| 装配 | `app/state.rs`、`app/scheduler.rs` | 状态机与事件循环 |
| 命令行 | `cli/mod.rs`、`cli/match_cmd.rs`、`cli/completion.rs`、`cli/config_cmd.rs`、`cli/log_cmd.rs`、`cli/uninstall.rs` | clap 定义命令树；补全脚本由同一份定义生成，不会和实现走偏 |

模块之间只靠 `Snapshot` 和一个 `matched_trigger` 字符串耦合。

### 触发与焦点

面板是 `WindowKind::PopUp`，在 macOS 上对应 `NSPanel` + `NonactivatingPanel`：浮在最前、跟随所有 Space，但不激活 App。目标 App 的光标位置全程不变，这是回传能「零折返」的前提——不需要还原前台，也不需要 espanso 官方那套 200ms 等待。

按键由 CGEventTap 吞下后直接改 `AppState`，不经过 gpui 的键盘事件，因此面板本身不接收焦点。

### 回传顺序

只有一种正确顺序，写在 `handoff::confirm` 里：

1. 面板 `orderOut`（由调用方先行关闭）
2. 回删被吞掉的触发符（`erase` 个）
3. `espanso match exec -t <trigger>`
4. 失败时：`allow_paste_fallback` 为真则安全渲染 + 剪贴板粘贴，否则只记日志

`match exec` 没有回执，失败只能靠退出码和 `espanso log`。

### 两条回传协议

- **snipshot → espanso**：只有一个 `espanso match exec -t <trigger>`。协议里唯一必须精确传递的是「命中了哪个 trigger」，不是片段 id。
- **espanso → snipshot**：仅在降级路径存在。没有辅助功能权限时，espanso 的 shell match 调用 `snipshot trigger --origin espanso` 唤醒面板。该 match 的 stdout 必须严格为空。

## 数据源

- 递归读取 `$ESPANSO_CONFIG/match/**/*.{yml,yaml}`
- `_` 开头的文件不加载（`_manifest.yml` 靠这条排除）
- `packages/` 下的片段同样会被收进来
- 启动时与 `espanso match list --json` 做一次交叉校验，以 espanso 眼中的 trigger 集合为准
- 监听 `match/` 目录，300ms 防抖后后台重建索引；也可以 `snipshot reload` 手动刷新
- 重建失败时保留旧快照，面板不会突然空掉

espanso 的 `filter_class` / `filter_exec` 这类 app-specific 规则目前**没有复刻**，靠 `match list --json` 的交叉校验兜底。

## 预览的安全边界

预览与展开是两条完全不同的路径。展开由 espanso 真求值；预览只做无副作用的近似：

| 变量类型 | 预览行为 |
|---|---|
| `echo` | 显示常量值 |
| `date` | 按 `format` / `offset` 实际算出日期 |
| `random` | 显示候选列表 |
| `clipboard` | 只显示字符数（`preview_clipboard` 为真时才显示前 80 字） |
| `shell` / `script` | **不执行**，占位符原样保留，并显示红色告警与命令首行 |
| `choice` / `form` | 显示候选数量 / 提示会弹表单 |
| 未知类型 | 保留占位符并告警 |

## 故障排查

先跑 `snipshot doctor`，再看日志：

```bash
snipshot log --level warn -n 50    # 只看警告与错误
snipshot log -f                    # 复现问题时盯着
```

| 现象 | 原因 | 处理 |
|---|---|---|
| 敲 `;;` 没反应 | 辅助功能未授权，且 espanso 降级 match 还没生效 | 授权后 `espanso restart` |
| 面板出现但打字没反应 | tap 没装上，按键落到了目标 App | 检查 `doctor` 的辅助功能一项 |
| 面板里有字，目标 App 也有字 | tap 放行了事件 | 确认 `trigger_timeout_ms` 内完成两次按键 |
| 回车没展开 | espanso 路径错误 / `match exec` 失败 | 看日志里的 `match exec 失败`，确认 `which espanso` |
| 片段 0 条 | 配置目录找错 | `espanso path config` 应返回目录；`espanso path base` 返回的是 `base.yml` **文件**，不能当目录用 |
| 文档里多出一个 `;` | `erase` 与实际被吞的键数不一致 | 见下方「已知限制」第一条 |
| 列表顺序奇怪 | `usage.json` 里的使用统计 | 删掉该文件即可重置 |
| `doctor` 说「当前进程有授权，但 daemon 没有」 | TCC 按进程判：终端跑的命令继承终端授权，launchd 起的 daemon 是独立条目；ad-hoc 重签名后条目会失效 | 系统设置 → 隐私与安全性 → 辅助功能，删掉旧的 Snipshot 再重新添加（或重新勾选） |
| 日志里每 10 秒一条「已有 daemon 在运行」 | 老版本抢 socket 失败时退出码为 1，launchd 的 `KeepAlive` 于是不停重启 | 已在 0.1.x 修掉（该情况改为退出码 0）；重装即可 |
| 改完 `config` 没生效 | daemon 启动时只读一次配置 | 按提示 `launchctl kickstart -k gui/$(id -u)/com.snipshot.daemon` |

## 开发

```bash
cargo test                       # 52 项单测：打分、拼音、预览、协议、YAML 解析、配置校验、日志过滤、卸载白名单
cargo clippy --all-targets
cargo fmt

RUST_LOG=debug ./target/debug/snipshot daemon   # 前台跑 daemon，日志同时进文件
./target/debug/snipshot trigger                 # 另一个终端打开面板
./target/debug/snipshot type "log"              # 合成输入
./target/debug/snipshot key down enter          # 合成按键
```

调试时建议先开一个纯文本窗口（TextEdit）作为注入目标，方便验证「展开位置是否准确」。

打包：

```bash
./scripts/bundle-macos.sh            # 只产出 target/Snipshot.app
./scripts/bundle-macos.sh --install  # 安装到 /Applications + LaunchAgent + CLI 软链
```

## 已知限制

- **非激活面板拿不到 IME 会话**。过滤走的是物理键码映射，所以不能用输入法打中文过滤词；中文标签靠拼音全拼/首字母索引弥补。真要输入法，需要 `focus_mode = "activating"`，该分支目前只是配置预留，未实现（还需要补「还原前台 + 等待」的折返）。
- **`erase` 依赖「espanso 看不到触发键」这一前提**。如果某个 espanso 版本也去消费 `;;`，并且它的 tap 排在 snipshot 前面，espanso 会自己抹掉 `;;`，而 snipshot 仍会回删 1 个字符，正好吃掉光标前的一个字。目前只是观察结论，未做确定性防护。
- **`match exec` 无回执**，失败只能看日志；粘贴兜底会覆盖剪贴板。
- **app-specific 过滤规则未复刻**，`filter_class` / `filter_exec` 场景下的候选可能比 espanso 原生搜索框多。
- **中文输入法下触发串可能变成全角**。`；；` 与 ASCII `;;` 不是同一个字符串，espanso 的匹配与回删又发生在输入法提交之前，实测可能残留全角触发符。触发时切到英文输入法最稳。
- **仅 macOS**。
- **ad-hoc 签名**，分发需要 Developer ID。

## License

MIT
