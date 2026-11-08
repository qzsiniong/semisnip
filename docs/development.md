# 开发

## 常用命令

```bash
cargo test                       # 122 项单测
cargo clippy --all-targets
cargo fmt
```

单测覆盖的是纯逻辑层：打分、拼音、查询解析、预览渲染、IPC 协议、YAML 解析、配置校验、日志过滤、卸载白名单，以及 `mark.rs` 的字形几何和 `tray.rs` 的菜单结构——**不需要 GUI，也不碰真实 espanso**。

## 端到端模拟测试

`src/sim_tests.rs` 是一条进程内的全链路：真触发状态机 + 真 `handoff::confirm_with` + 假文档 / 假 espanso，完整跑通「打字 → 触发 → 面板 → 确认 → 回传 → 展开」，**不需要 espanso、不需要辅助功能授权**。它守护的不变量：

- 触发串一个键都不吞；取消 = 纯关闭、零合成按键；
- 确认回删数与文档里躺着的触发符个数严格相等——tap 路径是触发串全长，降级路径是 0（espanso 已经把它擦了）；
- 占位字符（`config.placeholder` 五种取值都算）不受补偿退格到达时机影响（在途注入可以延迟到任意时刻结算，包括面板开着的时候）——这是「espanso 原生触发与面板确认交替使用互不干扰」的回归防线；
- 外部注入不进触发状态机。

改回传顺序、tap 路由规则或降级匹配格式时，先跑这组测试。往 `FakeEspanso` 加表项就能扩展场景；面板内部的过滤/选中逻辑不在这里测（`AppState::on_key` 有独立单测）。

`known_defect_degraded_match_eats_trailing_semicolon` 是**已知缺陷的现场记录**（tap 在线时，文档以 `;` 结尾后按 `;;` 会让降级匹配凑出 `;;;` 并擦掉正文里的分号）。它断言的是现状，不是期望；修好后要改成断言文档不动。

## 本地调试

```bash
RUST_LOG=debug ./target/debug/semisnip daemon   # 前台跑 daemon，日志同时进文件
./target/debug/semisnip trigger                 # 另一个终端打开面板
./target/debug/semisnip type "log"              # 合成输入
./target/debug/semisnip key down enter          # 合成按键
```

建议先开一个纯文本窗口（TextEdit）作为注入目标，方便验证「展开位置是否准确」。

`type` / `key` 在没有辅助功能权限时特别有用：它们直接向系统合成键盘事件，能单独验证「面板收到键之后做了什么」，把「tap 有没有装上」这个变量摘出去。

反过来，想看**没授权时长什么样**，用这个开关：

```bash
SEMISNIP_NO_TAP=1 ./target/debug/semisnip daemon   # 强制降级，面板会画「需要授权」页
```

开发机永远是已授权状态，那一页平时根本走不到。开关同时会让看门狗不再重试安装，日志里一眼能看出是调试模式（`SEMISNIP_NO_TAP 已设置`）。

## 构建期会写文件

`cargo build`（含增量构建）会跑 `build.rs`，把图标渲染到 `packaging/macos/`：

| 文件 | 是否入库 |
|---|---|
| `AppIcon.icns` | 忽略（构建产物） |
| `icon-preview.png` / `tray-preview.png` | 入库（文档配图） |

内容没变时不重写，所以不会无谓刷 mtime。改 `src/mark.rs` 里任何常量，下次构建三张图一起更新——应用图标是双色（字母近白、分号琥珀），菜单栏那张预览是单色，两者同源。

## 配置 schema 别忘同步

`assets/config.schema.json` 会在编译期被 `include_str!` 嵌进二进制，装好后写到 `config.yml` 旁边给编辑器用。改 `Config` 的字段、默认值、取值区间时**必须同时改它**——`schema_file_matches_the_struct` 会比对字段集合、每个 `default`、以及是否都有 `type` / `description`，漏改直接测试失败。

## 打包

```bash
./scripts/bundle-macos.sh             # 只产出 target/Semisnip.app（本机架构）
./scripts/bundle-macos.sh --universal # arm64 + x86_64 合成通用二进制
./scripts/bundle-macos.sh --dist      # 顺带出 target/Semisnip-<版本>-<架构>.{dmg,zip}
./scripts/bundle-macos.sh --install   # 安装到 /Applications + LaunchAgent + CLI 软链
```

版本号由脚本从 `Cargo.toml` 注入 `Info.plist`（在 `codesign` 之前），所以 `.app` 显示的版本永远等于刚构建的二进制。细节（签名、权限、LaunchAgent）见 [安装](install.md)。

发版走 `scripts/release.sh`，CI 与 tag 发版流水线见 [发版](release.md)。

## 改动方向上的约定

- **形状只有一份**：图标与菜单栏标记都来自 `mark.rs`，不要在别处再画一遍。
- **搜索是纯函数**：`search/` 里不允许出现 IO，排序要确定性可测。
- **回传顺序只有一份**：`handoff::confirm_with` 是唯一实现，`sim_tests` 驱动同一份；不要在测试里复制序列逻辑。
- **预览不执行任何东西**：`preview/` 只读 `&Snippet`，新增变量类型时必须保持「不求值」。
- **状态变更走事件**：tray、tap、CLI 都往 scheduler 的事件通道里投事件，不要在回调里直接改 `AppState`。

## 注释与文档的约定

这份代码的目标是「半年后回来还能读懂为什么」，所以注释写到能解释**决策**为止。

**注释写到什么程度**

| 位置 | 要求 |
|---|---|
| 文件顶部 | `//!` 说明这个模块负责什么、**不负责什么**、和相邻模块的边界 |
| 每个 `pub` 项 | `///` 说明做什么、参数/返回的含义与约束、在什么时机被调用 |
| 非显然的实现 | 写「为什么这样做」，特别是为绕过什么坑；不要复述代码在干什么 |
| 平台坑 | 就地留注释（TCC 授权按进程、bash 3.2 的多字节变量名、gpui 的 API 限制……），下次别再踩 |
| 魔法数字 | 说明它是量出来的还是拍的，以及改大改小会怎样 |

反例是「循环遍历列表并过滤」这种把代码翻译一遍的注释——它不提供任何代码里没有的信息，只会随代码一起腐烂。判断标准：删掉这条注释，读者会不会少知道一件事。

**改代码时必须同时改的地方**

1. 该模块的文档注释（函数语义变了，注释当次改，不留 TODO）；
2. 用户可见行为变化 → `docs/*.md` 与 `README.md`；
3. 新增/改名/删除文档 → README 的文档表；
4. 改 `Config` 字段 → `assets/config.schema.json`（有测试兜底）。

改完跑一遍门禁，并确认文档链接没有断：

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```
