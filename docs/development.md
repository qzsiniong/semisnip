# 开发

## 常用命令

```bash
cargo test                       # 77 项单测
cargo clippy --all-targets
cargo fmt
```

单测覆盖的是纯逻辑层：打分、拼音、查询解析、预览渲染、IPC 协议、YAML 解析、配置校验、日志过滤、卸载白名单，以及 `mark.rs` 的字形几何和 `tray.rs` 的菜单结构——**不需要 GUI，也不碰真实 espanso**。

## 本地调试

```bash
RUST_LOG=debug ./target/debug/snipshot daemon   # 前台跑 daemon，日志同时进文件
./target/debug/snipshot trigger                 # 另一个终端打开面板
./target/debug/snipshot type "log"              # 合成输入
./target/debug/snipshot key down enter          # 合成按键
```

建议先开一个纯文本窗口（TextEdit）作为注入目标，方便验证「展开位置是否准确」。

`type` / `key` 在没有辅助功能权限时特别有用：它们直接向系统合成键盘事件，能单独验证「面板收到键之后做了什么」，把「tap 有没有装上」这个变量摘出去。

## 构建期会写文件

`cargo build`（含增量构建）会跑 `build.rs`，把图标渲染到 `packaging/macos/`：

| 文件 | 是否入库 |
|---|---|
| `AppIcon.icns` | 忽略（构建产物） |
| `icon-preview.png` / `tray-preview.png` | 入库（文档配图） |

内容没变时不重写，所以不会无谓刷 mtime。改 `src/mark.rs` 里任何常量，下次构建三张图一起更新。

## 配置 schema 别忘同步

`assets/config.schema.json` 会在编译期被 `include_str!` 嵌进二进制，装好后写到 `config.yml` 旁边给编辑器用。改 `Config` 的字段、默认值、取值区间时**必须同时改它**——`schema_file_matches_the_struct` 会比对字段集合、每个 `default`、以及是否都有 `type` / `description`，漏改直接测试失败。

## 打包

```bash
./scripts/bundle-macos.sh            # 只产出 target/Snipshot.app
./scripts/bundle-macos.sh --install  # 安装到 /Applications + LaunchAgent + CLI 软链
```

细节（签名、权限、LaunchAgent）见 [安装](install.md)。

## 改动方向上的约定

- **形状只有一份**：图标与菜单栏标记都来自 `mark.rs`，不要在别处再画一遍。
- **搜索是纯函数**：`search/` 里不允许出现 IO，排序要确定性可测。
- **预览不执行任何东西**：`preview/` 只读 `&Snippet`，新增变量类型时必须保持「不求值」。
- **状态变更走事件**：tray、tap、CLI 都往 scheduler 的事件通道里投事件，不要在回调里直接改 `AppState`。
