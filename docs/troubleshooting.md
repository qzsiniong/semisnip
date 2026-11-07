# 故障排查

先跑 `snipshot doctor`，再看日志：

```bash
snipshot log -l warn -n 50    # 只看警告与错误
snipshot log -f               # 复现问题时盯着
```

## 现象对照

| 现象 | 原因 | 处理 |
|---|---|---|
| 敲 `;;` 没反应 | 辅助功能未授权，且 espanso 降级 match 还没生效 | 授权后 `espanso restart` |
| 面板出现但打字没反应 | tap 没装上，按键落到了目标 App | 检查 `doctor` 的辅助功能一项 |
| 面板里有字，目标 App 也有字 | tap 放行了事件 | 确认 `trigger_timeout_ms` 内完成两次按键 |
| 回车没展开 | espanso 路径错误 / `match exec` 失败 | 看日志里的 `match exec 失败`，确认 `which espanso` |
| 片段 0 条 | 配置目录找错 | `espanso path config` 应返回目录；`espanso path base` 返回的是 `base.yml` **文件**，不能当目录用 |
| 文档里多出一个 `;` | `erase` 与实际被吞的键数不一致 | 见下方「已知限制」第二条 |
| 列表顺序奇怪 | `usage.json` 里的使用统计 | 删掉该文件即可重置 |
| `doctor` 说「当前进程有授权，但 daemon 没有」 | TCC 按进程判：终端跑的命令继承终端授权，launchd 起的 daemon 是独立条目；ad-hoc 重签名后条目会失效 | 系统设置 → 隐私与安全性 → 辅助功能，删掉旧的 Snipshot 再重新添加 |
| 日志里每 10 秒一条「已有 daemon 在运行」 | 老版本抢 socket 失败时退出码为 1，launchd 的 `KeepAlive` 于是不停重启 | 已在 0.1.x 修掉（该情况改为退出码 0）；重装即可 |
| 改完 `config` 没生效 | daemon 启动时只读一次配置 | `launchctl kickstart -k gui/$(id -u)/com.snipshot.daemon` |
| 配置好像被忽略了 | YAML 写坏、类型不对，`Config::load` 退回默认值 | `snipshot log -l warn` 里找「配置解析失败」，报错带行列号；或 `snipshot config edit` 退出时会校验 |
| 菜单栏看不到图标 | 菜单栏排满，被折叠进溢出区 | 见 [菜单栏与图标 · 图标没出现？](tray.md#图标没出现) |

## 已知限制

- **非激活面板拿不到 IME 会话**。过滤走的是物理键码映射，所以不能用输入法打中文过滤词；中文标签靠拼音全拼/首字母索引弥补。真要输入法，需要 `focus_mode = "activating"`，该分支目前只是配置预留，未实现（还需要补「还原前台 + 等待」的折返）。
- **`erase` 依赖「espanso 看不到触发键」这一前提**。如果某个 espanso 版本也去消费 `;;`，并且它的 tap 排在 snipshot 前面，espanso 会自己抹掉 `;;`，而 snipshot 仍会回删 1 个字符，正好吃掉光标前的一个字。目前只是观察结论，未做确定性防护。
- **`match exec` 无回执**，失败只能看日志；粘贴兜底会覆盖剪贴板。
- **app-specific 过滤规则未复刻**，`filter_class` / `filter_exec` 场景下的候选可能比 espanso 原生搜索框多。
- **中文输入法下触发串可能变成全角**。`；；` 与 ASCII `;;` 不是同一个字符串，espanso 的匹配与回删又发生在输入法提交之前，实测可能残留全角触发符。触发时切到英文输入法最稳。
- **仅 macOS**。
- **ad-hoc 签名**，分发需要 Developer ID；每次 rebuild 都要重新授权辅助功能。
