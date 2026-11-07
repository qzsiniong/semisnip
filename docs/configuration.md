# 配置

## 位置

```
~/Library/Application Support/snipshot/config.json
```

`snipshot install` 时生成；不存在时用默认值。写入策略是**先落临时文件再改名**，daemon 不会读到半截 JSON。

## 默认配置

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

## 怎么改

```bash
snipshot config                      # 当前值，与默认值不同的项标 *
snipshot config get trigger          # 取值（字符串不带引号，方便塞进脚本）
snipshot config set max_results 12   # 改一个键
snipshot config unset max_results    # 恢复默认
snipshot config reset                # 全部恢复默认
snipshot config keys                 # 列出所有键与含义
snipshot config edit                 # $EDITOR 打开，退出时自动校验 JSON
snipshot config path                 # 配置文件路径
```

`set` 先按默认值的类型解析（`true/false`、整数、小数、字符串），再做语义校验：

- `trigger` 必须能映射成物理键码（中文会直接报错）
- `max_results` 限 1–100
- `panel_width` / `panel_height` 限 200–2000

**配置在 daemon 启动时只读一次**。改完 `config set` 会打印完整的重启命令：

```bash
launchctl kickstart -k gui/$(id -u)/com.snipshot.daemon
```

## snipshot 自己有哪些文件

只有这四个，卸载不会碰到任何片段数据：

```
~/Library/Application Support/snipshot/config.json   偏好
~/Library/Application Support/snipshot/usage.json    使用频率
~/Library/Application Support/snipshot/snipshot.log  日志
~/Library/Caches/snipshot/snipshot.sock              daemon socket
```

片段永远只存在于 espanso 的 `match/**/*.yml` 里。snipshot 唯一写进那个目录的文件是 `snipshot.yml`（降级触发定义），见 [命令行 · match](cli.md#match)。
