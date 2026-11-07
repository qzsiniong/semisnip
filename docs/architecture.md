# 架构

## 数据流

```mermaid
flowchart TB
    key["任意 App 输入框<br/>敲 ;;"] --> tap["CGEventTap<br/>只认物理键码"]
    cli["semisnip trigger<br/>UDS + NDJSON"]
    tray["菜单栏图标<br/>GPUI action"]
    yml["$ESPANSO_CONFIG<br/>match/**/*.yml"]

    subgraph core["daemon：常驻进程，不进 Dock / Cmd-Tab"]
        direction TB
        state["AppState<br/>状态机 + 事件通道"]
        idx["index<br/>ArcSwap&lt;Snapshot&gt;"]
        search["search<br/>分档打分 + 拼音"]
        preview["preview<br/>只读渲染，不执行 shell"]
        handoff["handoff<br/>orderOut → erase → exec"]
        idx --> search
        state --> search
        search --> preview
        state --> handoff
    end

    tap --> state
    cli --> state
    tray --> state
    yml --> idx
    handoff --> espanso["espanso<br/>match exec -t &lt;trigger&gt;"]
    espanso --> target["目标输入框<br/>光标位置从未改变"]
```

三条输入（tap 吞键、CLI 的 UDS 消息、菜单栏的 GPUI action）走的是同一个事件通道，所以「敲 `;;`」「`semisnip trigger`」「点菜单」在 daemon 看来没有区别。daemon 内部单向：索引 → 检索 → 预览，选中后交给回传。模块之间只靠 `Snapshot` 和一个 `matched_trigger` 字符串耦合。

## 模块与边界

| 模块 | 文件 | 职责与边界 |
|---|---|---|
| 触发 | `trigger/tap.rs`、`trigger/machine.rs`、`trigger/keymap.rs` | 只认物理键码，不知道片段存在；`;;` 的第二个键被吞、第一个键记为"已在文档里"的残留。同一个 tap 还订阅鼠标按下——只为「点面板外 = 收起面板」，判定靠开窗时记住的窗口矩形，鼠标事件永远放行 |
| 通信 | `ipc/proto.rs`、`ipc/server.rs`、`ipc/client.rs` | UDS + NDJSON，带版本号；单实例，daemon 不在时由 CLI 自愈拉起 |
| 数据 | `index/loader.rs`、`index/service.rs` | 唯一读 YAML 的地方，产出 `Arc<Snapshot>`；后台重建，UI 永远只读 |
| 检索 | `search/score.rs`、`search/query.rs`、`search/pinyin.rs` | 纯函数、零 IO、确定性排序 |
| 预览 | `preview/vars.rs` | 只吃 `&Snippet`，**绝不执行 shell/script** |
| 回传 | `handoff/mod.rs`、`handoff/inject.rs` | 只认 `Hit` 与 trigger 字符串，不关心 YAML 结构 |
| espanso | `espanso/cli.rs` | 定位可执行文件；`match exec` / `match list --json` 的唯一出口 |
| 路径 | `paths.rs` | 全项目唯一知道「东西放哪」的文件：系统目录问 `dirs`，自己的文件、`/Applications`、`/usr/local/bin`、LaunchAgent、espanso 的 `match/` 全在这里。业务模块只许调用，不许拼字面量 |
| 配置 | `config.rs`、`cli/config_cmd.rs` | YAML + 内嵌 JSON Schema（`assets/config.schema.json`）；只碰自己的偏好，不碰 espanso 的 YAML |
| 定位 | `platform.rs`、`ui/place.rs` | `platform` 只回答两个问题：每块屏在全局坐标里的矩形、插入点在哪（AX → 单行控件 → 鼠标三级退化）；`place` 把答案翻译成 gpui 要的「屏幕本地坐标 + DisplayId」，其中 `place_auto` / `place_center` 是纯函数、可单测。全局 ↔ 本地的换算只发生在这一层 |
| 界面 | `ui/window.rs`、`ui/view.rs` | 面板：`WindowKind::PopUp`（NSPanel + NonactivatingPanel），不处理键盘；右上角 `✕` 是面板内唯一的可点元素，点面板外由 tap 的鼠标监听收起——两路都只投事件，收尾逻辑仍归 scheduler。`tap_ok` 为假时整块换成「需要授权」页——没接上键盘还画一列候选，等于骗人 |
| 设置 | `ui/settings.rs` | 普通窗口 + gpui-component 的 `Settings`；字段直接读写一份全局 `SettingsState`，改动立即落盘并投 `ConfigChanged` |
| 引导 | `ui/onboarding.rs` | 四步首次引导；体检是打开时的一次快照（`espanso --version` 要起进程），走完写 `onboarded` |
| 窗口公共 | `ui/mod.rs` | 主题跟随系统外观（`sync_theme`）、`reveal()`、打开辅助功能设置页 |
| 菜单栏 | `tray.rs` | NSStatusItem + 原生菜单；模板图（深浅色自动反色），菜单项经 GPUI action 回到 scheduler 的事件通道。第一项按窗口句柄在「打开 / 关闭片段面板」之间切换——tap 不可用时它是收面板的唯一出口，所以开合之后要 `refresh` |
| 图标 | `mark.rs` | 「斜体 `I` + `;;`」的解析式几何；`sample()` 会区分字母与触发符，所以应用图标能双色、菜单栏仍走单色模板图。运行期与构建期编译的是**同一份文件** |
| 装配 | `app/state.rs`、`app/scheduler.rs`、`app/event.rs` | 状态机与事件循环；`Event::ConfigChanged` 是"窗口改了配置 → 面板换值"的唯一通道；另有 tap 看门狗：每秒对齐 `tap_ok`，没就绪就每 5 秒试一次重装（授权补上之后不用重启 daemon） |
| 命令行 | `cli/mod.rs`、`cli/match_cmd.rs`、`cli/completion.rs`、`cli/config_cmd.rs`、`cli/log_cmd.rs`、`cli/uninstall.rs` | clap 定义命令树；补全脚本由同一份定义生成，不会和实现走偏 |

## 触发与焦点

面板是 `WindowKind::PopUp`，在 macOS 上对应 `NSPanel` + `NonactivatingPanel`：浮在最前、跟随所有 Space，但不激活 App。目标 App 的光标位置全程不变，这是回传能「零折返」的前提——不需要还原前台，也不需要 espanso 官方那套 200ms 等待。

按键由 CGEventTap 吞下后直接改 `AppState`，不经过 gpui 的键盘事件，因此面板本身不接收焦点。

代价是拿不到 IME 会话：过滤只能走物理键码映射，中文过滤词要靠拼音索引。见 [故障排查 · 已知限制](troubleshooting.md#已知限制)。

面板不抢焦点，也意味着**点别处不会自动收起来**：`NonactivatingPanel` 不会收到 `resignKey`，光标从来没在它身上。所以收面板要靠自己判：tap 订阅了鼠标按下，拿 `CGEventGetLocation` 与开窗时记下的窗口矩形比一比，落在外面就投 `Event::ClickOutside`。两个细节值得写下来：

- 鼠标事件**一律放行**，绝不吞——面板里还要点 `✕`、点授权页的按钮，抢了点击就全废了；
- 这一路收尾**不动目标文档**。点下去的那一刻前台窗口正在换人，合成出来的退格或补字符可能落到新窗口里；键盘那几条路（`Esc`、`✕`、菜单栏）前台没变，才敢把触发词补回去。

## 回传顺序

只有一种正确顺序，写在 `handoff::confirm` 里：

1. 面板 `orderOut`（由调用方先行关闭）
2. 回删被吞掉的触发符（`erase` 个）
3. `espanso match exec -t <trigger>`
4. 失败时：`allow_paste_fallback` 为真则安全渲染 + 剪贴板粘贴，否则只记日志

```mermaid
sequenceDiagram
    autonumber
    participant U as 用户
    participant P as 面板
    participant S as AppState
    participant H as handoff
    participant E as espanso
    U->>P: Enter
    P->>S: 选中项的 trigger
    S->>H: confirm(hit, erase)
    H->>P: orderOut
    H->>H: 回删 erase 个触发符
    H->>E: match exec -t trigger
    alt 成功
        E-->>U: 求值后注入目标输入框
    else 失败
        H->>H: 安全渲染 + 剪贴板粘贴
    end
```

`match exec` 没有回执，失败只能靠退出码和 `espanso log`。

**取消走相反的方向。** `handoff::keep_trigger` 不想让光标"干净"，而是要让状态回到"没触发过"：把 tap 吞掉的那个键补回去，`;;` 原样留在文档里。两条路都只能通过 `handoff` 合成按键，所以写在一个模块里——方向写反的表现是"按 Esc 吃掉半截触发词"或"回车之后文档里多一个分号"。

## 两条回传协议

- **semisnip → espanso**：只有一个 `espanso match exec -t <trigger>`。协议里唯一必须精确传递的是「命中了哪个 trigger」，不是片段 id。
- **espanso → semisnip**：仅在降级路径存在。没有辅助功能权限时，espanso 的 shell match 调用 `semisnip trigger --origin espanso` 唤醒面板。该 match 的 stdout 必须严格为空。

## 数据源

- 递归读取 `$ESPANSO_CONFIG/match/**/*.{yml,yaml}`
- `_` 开头的文件不加载（`_manifest.yml` 靠这条排除）
- `packages/` 下的片段同样会被收进来
- 启动时与 `espanso match list --json` 做一次交叉校验，以 espanso 眼中的 trigger 集合为准
- 监听 `match/` 目录，300ms 防抖后后台重建索引；也可以 `semisnip reload` 手动刷新
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

## 菜单栏与图标

daemon 不进 Dock、不进 Cmd-Tab，只留一个菜单栏图标。见 [菜单栏与图标](tray.md)。
