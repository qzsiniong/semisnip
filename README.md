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

脚本会依次做：release 构建 → 组装 `Snipshot.app` → ad-hoc 签名 → 拷到 `/Applications` → 注册 LaunchAgent → 写入 `$ESPANSO_CONFIG/match/snipshot.yml`。

之后还有两步必须手动完成（系统不允许脚本代劳）：

1. **系统设置 → 隐私与安全性 → 辅助功能** → 勾选 `Snipshot`
2. `espanso restart`

最后体检：

```bash
snipshot doctor
```

四项全绿即可使用。`doctor` 会检查 espanso 可执行文件、配置目录、片段索引、daemon 存活与辅助功能授权。

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

```
snipshot                            启动 daemon（默认）
snipshot daemon                     同上
snipshot trigger [--origin O]       请求打开面板（origin: tap | espanso | manual）
snipshot reload                     重建索引
snipshot install                    写入 espanso 触发定义并打印后续步骤
snipshot doctor                     体检
snipshot type <text>                调试：合成字符输入
snipshot key <esc|enter|up|down>    调试：合成命名按键
```

`trigger` 被 espanso 的 shell 变量同步调用，因此**成功时 stdout 必须为空**——espanso 会把 stdout 原样注入文档，多一个字节就是污染。所有错误都走 stderr。

`type` / `key` 会向系统合成真实键盘事件，仅用于没有辅助功能权限时的冒烟测试。

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
| 装配 | `app/state.rs`、`app/scheduler.rs` | 状态机与事件循环 |

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

```
~/Library/Application Support/snipshot/snipshot.log
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

## 开发

```bash
cargo test                       # 37 项单测：打分、拼音、预览、协议、YAML 解析
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
./scripts/bundle-macos.sh --install  # 安装到 /Applications 并注册 LaunchAgent
```

## 已知限制

- **非激活面板拿不到 IME 会话**。过滤走的是物理键码映射，所以不能用输入法打中文过滤词；中文标签靠拼音全拼/首字母索引弥补。真要输入法，需要 `focus_mode = "activating"`，该分支目前只是配置预留，未实现（还需要补「还原前台 + 等待」的折返）。
- **`erase` 依赖「espanso 看不到触发键」这一前提**。如果某个 espanso 版本也去消费 `;;`，并且它的 tap 排在 snipshot 前面，espanso 会自己抹掉 `;;`，而 snipshot 仍会回删 1 个字符，正好吃掉光标前的一个字。目前只是观察结论，未做确定性防护。
- **`match exec` 无回执**，失败只能看日志；粘贴兜底会覆盖剪贴板。
- **app-specific 过滤规则未复刻**，`filter_class` / `filter_exec` 场景下的候选可能比 espanso 原生搜索框多。
- **仅 macOS**。
- **ad-hoc 签名**，分发需要 Developer ID。

## License

MIT
