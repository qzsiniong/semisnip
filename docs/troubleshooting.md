# 故障排查

先跑 `semisnip doctor`，再看日志：

```bash
semisnip log -l warn -n 50    # 只看警告与错误
semisnip log -f               # 复现问题时盯着
```

## 现象对照

| 现象 | 原因 | 处理 |
|---|---|---|
| 面板跑到另一块屏 / 位置不在光标附近 | 插入点取不到（终端、部分 Electron 应用不暴露 AX 光标），于是退到鼠标；或者取到的矩形不可信被丢弃 | 看日志 `面板已打开 … · 跟随插入点（来源）· 屏 N`：来源是「鼠标位置」就说明 AX 没给答案。想要稳定位置就把 `position` 改成 `center` |
| 面板位置偏了一点（贴着光标但差半行） | 各 App 报的插入点矩形精度不同（TextEdit / Safari 地址栏会把 y 报成上一行） | 已做相邻字符校正；仍有偏差时属于应用侧数据问题，改 `position: center` 可绕开 |
| 双击 `Semisnip.app` 没反应 | 旧版本对"没有参数"一律只打印帮助，而 Finder 双击恰好也不带参数——帮助打在 `/dev/null` 里，观感就是"图标闪一下" | 新版会按来源区分：双击 → 起 daemon / 已有实例则弹面板，终端裸敲才打印帮助（见 [命令行](cli.md)）。没升级就手动 `semisnip daemon` |
| 敲 `;;` 没反应 | 辅助功能未授权，且 espanso 降级 match 还没生效（降级触发串是 `;;;`，比正常多打一个末字符） | 授权后 `espanso restart`；降级 match 旧了就 `semisnip match regenerate` |
| 敲 `;;` 完全没反应，菜单栏图标是「划掉」的 | 触发被暂停了（菜单栏「暂停触发」或偏好设置里的「启用触发」） | 菜单栏点「恢复触发 `;;`」，或偏好设置 → 通用 → 打开「启用触发」。这是有意为之的状态，不是故障；此时 `;;;`（降级触发）仍然能开面板 |
| 敲 `;;;`（降级触发）完全没反应 | 链路有三段，断哪段都表现为"没反应"：espanso 没加载降级 match / CLI 连不上 daemon / daemon 收到却没开窗 | 看日志：出现「降级唤醒已排队」说明事件到了，约 150ms 后应紧跟「打开面板」；两条都没有就是 espanso 侧没跑起来（`semisnip match check` 看文件，再 `espanso restart`） |
| 面板只显示「需要辅助功能」授权页 | `tap` 没装上，daemon 收不到任何按键——这时画候选列表就是骗人，所以整块换成了授权页 | 按页上三步做：打开设置 → 勾上 Semisnip → 点「重新检查」。授权一到，面板几秒内自己变回列表 |
| 面板出现但打字没反应 | 同上：按键落到了目标 App，daemon 一个键都没收到 | 看 `doctor` 的辅助功能一项；面板本身会告诉你（见上一行） |
| 面板关不掉（`Esc` 无效） | 同上：面板不处理键盘，`Esc` 只走 tap | 点面板右上角 `✕`，或菜单栏第一项「关闭片段面板」；根治还是补授权 |
| 点外面关不掉 | 鼠标判定也装在同一个 tap 上（顺带用它的辅助功能授权），tap 不在就一起失效；另一个原因是开窗时没量到窗口矩形，日志里会有「量不到面板位置」 | 同上：补授权；量不到位置是 gpui 侧的异常，重启 daemon 可恢复 |
| 面板里有字，目标 App 也有字 | tap 放行了事件 | 确认 `trigger_timeout_ms` 内完成两次按键 |
| 回车没展开 | espanso 路径错误 / `match exec` 失败 | 看日志里的 `match exec 失败`，确认 `which espanso` |
| 片段 0 条 | 配置目录找错 | `espanso path config` 应返回目录；`espanso path base` 返回的是 `base.yml` **文件**，不能当目录用 |
| 关掉面板后文档里留着 `;;` | 这是**故意的**：触发键一个不吞，文档里本来就是用户打的原样，取消（`Esc` / `✕` / 退格退到空 / 点外面）= 当作没触发过 | 想删就直接按两次退格；只有回车展开时才会清掉触发词 |
| 列表顺序奇怪 | `usage.json` 里的使用统计 | 删掉该文件即可重置 |
| `doctor` 说「当前进程有授权，但 daemon 没有」 | TCC 按进程判：终端跑的命令继承终端授权，launchd 起的 daemon 是独立条目；ad-hoc 重签名后条目会失效 | 系统设置 → 隐私与安全性 → 辅助功能，删掉旧的 Semisnip 再重新添加 |
| 日志里每 10 秒一条「已有 daemon 在运行」 | 老版本抢 socket 失败时退出码为 1，launchd 的 `KeepAlive` 于是不停重启 | 已在 0.1.x 修掉（该情况改为退出码 0）；重装即可 |
| 改完 `config` 没生效 | daemon 启动时只读一次配置 | `launchctl kickstart -k gui/$(id -u)/com.semisnip.daemon` |
| 配置好像被忽略了 | YAML 写坏、类型不对，`Config::load` 退回默认值 | `semisnip log -l warn` 里找「配置解析失败」，报错带行列号；或 `semisnip config edit` 退出时会校验 |
| 设置里改了触发键，`;;` 还是旧的 | espanso 的降级定义还是旧键（tap 正常时用不到它，但授权失效后会） | 跑一次 `semisnip install`，再 `espanso restart` |
| 设置里改了，面板没变 | 面板用的是打开那一刻的配置；尺寸类改动下次打开才生效 | 触发键/条数是立即生效的，尺寸要关掉面板重开 |
| 改了辅助功能还是显示未授权 | 授权只影响「之后创建」的 tap；不过 daemon 每秒会对齐一次状态、每 5 秒重试一次安装，所以多数情况几秒后自己就好了 | 等两秒；还不行就点面板上的「重新检查」。真卡住才需要 `launchctl kickstart -k gui/$(id -u)/com.semisnip.daemon` |
| 菜单栏看不到图标 | 菜单栏排满，被折叠进溢出区 | 见 [菜单栏与图标 · 图标没出现？](tray.md#图标没出现) |
| 打 `,,sig` 没展开，反而弹了面板 | `direct_prefix` 与 `trigger` 相等或互为前缀（非法配置被旧版本放过了），或者只改了 semisnip 配置、没迁移/安装 espanso 侧的触发词 | `semisnip config get trigger` 与 `direct_prefix` 对照；改完跑 `semisnip install` 再 `espanso restart`。管理面板里的迁移只改 YAML，espanso 也要重启才认 |
| 管理面板里的「毕业率」偏低 | 那是估算：tap 只看物理按键、不看 espanso 是否真的展开；密码框、安全输入、tap 未授权时完全不统计 | 不当成 bug 用；想看真实分布就同时看「面板」次数 |
| 迁移后 YAML 注释没了 | `migrate` 会在文件里追加触发词，序列化会丢注释（所以先备份、先预览） | 从 `~/Library/Application Support/semisnip/backups/<时间>/` 恢复原文件；要保留注释就手动加逐条改 |
| 导出文件里少了某条 | 导出按 `(文件, 序号)` 复制原始 match；索引重建后序号变化会让选中项对不上 | 重新打开管理面板再选一次；日志里会有「序号失配」的告警 |

## 占位符：展开时文档里为什么会多出几个字符

**这是 espanso 的限制，不是 semisnip 的选择。** espanso 在注入展开内容前，一定会按
**被选中片段 trigger 的字符数**发一串退格（`TriggerCompensation`，实现在
`espanso-engine/src/process/middleware/action.rs`：`backspace_count = trigger.chars().count()`），
而且没有关闭入口——`espanso match exec` 也会走这条路。

那些退格删的是**光标左边**的字符，可 semisnip 确认时文档里只有用户正文。所以必须在
调用 `match exec` 之前，先打出同样多的「一退格删一个」的字符垫在那里，让退格把它们
收走。垫什么字符由 `placeholder` 决定（设置 → 行为 → 占位字符）：

| 取值 | 打出去的东西 | 适合 | 坑 |
|---|---|---|---|
| `space`（默认） | `trigger_len` 个空格 | 绝大多数场景 | 空格是「有语义的空白」：行首连续空格可能被编辑器 tab 化、行尾空白可能被 trim、输入法组合态下空格是上屏键、有些编辑器把空格当自动补全的接受键——任何一种都会让字符数变少，补偿退格就会多吃正文 |
| `zero_width` | `trigger_len` 个 U+200B | 编辑器会改写空格时 | 不可见、不是 Unicode 空白（`trim` / `\s` 收不走）、不会被 tab 化；代价是个别 App 会过滤零宽字符，那样同样会少字符 |
| `trigger` | 片段触发串本身 | 想让「删掉的东西就是 espanso 认识的东西」 | **这条不调 `match exec`**，见下 |
| `custom` | `placeholder_char` 重复 | 上面都不合适时 | 必须恰好一个字符、且不能是换行/制表符（会触发自动缩进，数量就错了） |
| `none` | 什么都不打 | 只在 espanso 将来支持关闭补偿后 | **现在选它等于每次展开吃掉光标前 `trigger_len` 个真实字符** |

### `placeholder: trigger` 为什么不能调 `match exec`

espanso 的检测器只跳过**自己**注入的事件——`espanso-detect/src/mac/native.mm` 里把
`ESPANSO_EVENT_MARKER`（`-27469`）塞在事件坐标里做标记。semisnip 合成的按键在它
眼里和真键盘没区别，所以把片段触发串打出去的那一刻，espanso 的被动匹配就会自己命中、
自己按 trigger 全长回删、自己注入。

这套动作与 `match exec` 触发的补偿完全等价。**如果此时再调一次 `match exec`，同一段
文字会被展开两遍**（第二轮的补偿还会吃掉第一轮的展开结果）。所以这条模式下 semisnip
打完触发串就收工，返回 `Delegated`。

代价，选它之前想清楚：

- **成败不可知**。espanso 没在跑、或该片段被 app-specific 配置禁用，触发串就会留在
  文档里，semisnip 只能记日志（`已打出触发串，等 espanso 自己命中`），看不到结果。
- **触发串含非 ASCII 字符时会自动退回空格模式**（日志里有一条 warn）。semisnip 打非 ASCII 走的是 unicode 注入（键码为 0），espanso 未必认得出这是"用户打的字"：认不出，触发串就留在文档里；认得出，又会和 `match exec` 双打。所以这条模式下只委托纯 ASCII 触发串。
- 这串按键也会经过 semisnip 自己的键盘 tap（和其他合成事件一样），所以片段触发串里
  别包含面板触发串（例如面板是 `;;`、片段触发串以 `;;` 结尾会把面板又叫出来）。

### 怎么切、怎么查

```bash
semisnip config set placeholder zero_width     # 立刻写盘，重启 daemon 生效
semisnip config set placeholder custom
semisnip config set placeholder_char "·"       # 必须单个字符、非控制字符
```

设置窗口里改是立即生效的。日志里每次回传都有一行
`回传 <trigger>：erase=… pad=… placeholder=…`，`pad` 就是这次垫了多少个字符——
它应该恒等于片段触发串的字符数。

## 已知限制

- **非激活面板拿不到 IME 会话**。过滤走的是物理键码映射，所以不能用输入法打中文过滤词；中文标签靠拼音全拼/首字母索引弥补。真要输入法，需要 `focus_mode = "activating"`，该分支目前只是配置预留，未实现（还需要补「还原前台 + 等待」的折返）。
- **降级触发串比正常的多打一个末字符**（`;;` → `;;;`）。触发键不吞，触发串会完整到达 espanso；如果降级匹配也用 `;;`，每次开面板它都会命中并把分号擦掉。多出的字符通常在面板打开期间凑不齐（面板吞掉一切按键），但取消后再补一个末字符仍能让它命中——这条路径按"随时会被触发"设计（见 [架构 · 两条回传协议](architecture.md#两条回传协议)）。
- **降级路径取消面板不会补回 `;;;`**。espanso 命中降级匹配时会立刻按 trigger 全长回删，`;;;` 在你按第三个键的那一刻就没了；semisnip 全程没见过它，也就补不出来。tap 路径没有这个问题（触发键从不被吞）。降级匹配**不**把触发串注回文档，是因为注回会和面板的回删/垫空格抢光标——那个 bug 的表现是"文档多出 `;;;`、正文被删"（见 [架构 · 两条回传协议](architecture.md#两条回传协议)）。
- **降级路径的面板会晚约 150ms 出现**。espanso 的回删要等 shell 变量（`semisnip trigger`）退出才发，面板等这段时间过去再开，避免用户在回删落地前按回车、两家抢同一个光标。这是刻意的延迟，不是卡顿。
- **`match exec` 无回执**，失败只能看日志；粘贴兜底会覆盖剪贴板。
- **慢片段展开期间能看到占位符**。回传会先打出 `trigger_len` 个占位字符给 espanso 的补偿退格（见上方 [占位符](#占位符展开时文档里为什么会多出几个字符)），shell 变量跑几百毫秒时这几格肉眼可见、随后被收走；嫌碍眼就换成 `zero_width`。这期间**不要按退格**——会自己吃掉占位字符，让补偿退格多删真实字符。
- **app-specific 过滤规则未复刻**，`filter_class` / `filter_exec` 场景下的候选可能比 espanso 原生搜索框多。
- **中文输入法下触发串可能变成全角**。`；；` 与 ASCII `;;` 不是同一个字符串——不过触发键不吞，两个全角分号都会照常落进文档，取消/展开都不会额外吃字；只是 semisnip 的触发判定认物理键码，全角键位未必映射得上，触发不了时切到英文输入法最稳。
- **仅 macOS**。
- **ad-hoc 签名**，分发需要 Developer ID；每次 rebuild 都要重新授权辅助功能。
