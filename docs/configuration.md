# 配置

## 位置

```
~/Library/Application Support/semisnip/config.yml        配置
~/Library/Application Support/semisnip/config.schema.json  schema（给编辑器用）
```

`semisnip install` 时生成；不存在时用默认值。写入策略是**先落临时文件再改名**，daemon 不会读到半截 YAML。

## 长这样

```yaml
# semisnip 配置。daemon 只在启动时读一次，改完执行：
#   semisnip config set <key> <value>              # 推荐：带类型与范围校验
#   launchctl kickstart -k gui/$(id -u)/com.semisnip.daemon
# 改坏了也不至于起不来：解析失败时 daemon 会用默认值，并把原因写进日志。
# yaml-language-server: $schema=./config.schema.json
trigger: ;;
trigger_timeout_ms: 800
focus_mode: nonactivating
max_results: 9
panel_width: 720.0
panel_height: 420.0
post_exec_delay_ms: 12
preview_clipboard: false
show_detail: true
allow_paste_fallback: true
```

只写想改的键也行——其余字段按默认值补（`#[serde(default)]`）。空文件、只有注释的文件同样按默认值处理，删光内容不会让 daemon 起不来。

## 编辑器补全与校验

文件头那行是指令，不是普通注释：

```yaml
# yaml-language-server: $schema=./config.schema.json
```

装了 YAML 扩展的 VS Code、以及 JetBrains 系 IDE 会据此**补全键名、悬停显示说明、当场标红**：

- 键名拼错（`max_result`）→ 报错，因为 schema 里 `additionalProperties: false`
- 类型写错（`max_results: "很多"`）→ 报错
- 超范围（`max_results: 0`）→ 报错，schema 里写了 `minimum: 1`
- 枚举写错（`focus_mode: focus`）→ 报错并给出候选

schema 由二进制内嵌（`assets/config.schema.json`），`install` 与每次 `config` 写入都会把它落到配置旁边，所以**不依赖仓库目录**，也不怕 app 被搬走。查看路径：

```bash
semisnip config schema
```

用**相对路径**而不是绝对路径，是因为 `~/Library/Application Support` 里有空格，编辑器解析绝对路径容易歪。schema 与配置永远同目录，相对引用最稳。

`schema` 与代码的一致性有单测兜底：字段集合、每项 `default`、是否都有 `description` / `type` 都会比一遍，改结构体忘了改 schema 会直接测试失败。

## 字段

| 字段 | 默认 | 说明 |
|---|---|---|
| `trigger` | `";;"` | 只允许 ASCII，最多 4 个字符，会被编译成物理键码序列 |
| `trigger_timeout_ms` | `800` | 两个触发键之间的最大间隔（200–3000） |
| `focus_mode` | `"nonactivating"` | 预留项；`activating` 分支尚未实现 |
| `max_results` | `9` | 候选条数上限（1–100） |
| `panel_width` / `panel_height` | `720` / `420` | 面板尺寸（200–2000） |
| `post_exec_delay_ms` | `12` | 回删到 `match exec` 之间的等待 |
| `preview_clipboard` | `false` | 详情区是否显示剪贴板全文；`false` 时只显示字符数 |
| `show_detail` | `true` | 是否显示右侧详情区 |
| `allow_paste_fallback` | `true` | `match exec` 失败时用「安全渲染 + 剪贴板粘贴」兜底 |

## 怎么改

```bash
semisnip config                      # 当前值，与默认值不同的项标 *
semisnip config get trigger          # 取值（字符串不带引号，方便塞进脚本）
semisnip config set max_results 12   # 改一个键
semisnip config unset max_results    # 恢复默认
semisnip config reset                # 全部恢复默认
semisnip config keys                 # 列出所有键与含义
semisnip config edit                 # $EDITOR 打开，退出时校验 YAML
semisnip config path                 # 配置文件路径
semisnip config schema               # schema 路径
```

`set` 先按默认值的类型解析（`true/false`、整数、小数、字符串），再做语义校验：

- `trigger` 必须能映射成物理键码（中文会直接报错）
- `max_results` 限 1–100
- `panel_width` / `panel_height` 限 200–2000

`set` / `unset` 会**整份重写**文件，除文件头外你手写的注释会丢；想保留注释就用 `config edit`，或者直接改文件——schema 会在编辑器里拦住写错的情况。

**配置在 daemon 启动时只读一次**。改完 `config set` 会打印完整的重启命令：

```bash
launchctl kickstart -k gui/$(id -u)/com.semisnip.daemon
```

## 从旧的 config.json 迁移

0.1 之前的版本写的是 `config.json`。首次读取时（daemon 启动或任意 `semisnip config` 命令）会自动迁移：

1. 解析 `config.json`
2. 按新格式写 `config.yml`（带抬头与 schema 提示）
3. 旧文件改名 `config.json.bak` 留档，**不删**

迁移不能省：静默退回默认值等于「用户没动它，trigger 却变回去了」，而 trigger 一变，espanso 的降级 match 就对不上。

## semisnip 自己有哪些文件

```
~/Library/Application Support/semisnip/config.yml          偏好
~/Library/Application Support/semisnip/config.schema.json  编辑器补全用
~/Library/Application Support/semisnip/usage.json          使用频率（内部格式，不用手改）
~/Library/Application Support/semisnip/semisnip.log        日志
~/Library/Caches/semisnip/semisnip.sock                    daemon socket
```

片段永远只存在于 espanso 的 `match/**/*.yml` 里。semisnip 唯一写进那个目录的文件是 `semisnip.yml`（降级触发定义），见 [命令行 · match](cli.md#match)。
