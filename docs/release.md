# 发版

## 版本号只有一处

`Cargo.toml` 的 `[package].version`。其余地方**都是派生**的，不要在别处手动维护：

| 出现位置 | 怎么来的 |
|---|---|
| `Semisnip.app/Contents/Info.plist` 的 `CFBundleShortVersionString` | `bundle-macos.sh` 打包时用 `PlistBuddy` 注入（在 `codesign` 之前） |
| `semisnip --version` | clap 的 `version` 属性，取自 Cargo.toml |
| Release 页面的 tag、资产文件名 | `release.sh` 打 tag，工作流拿它与 Cargo.toml 比对 |
| `Cargo.lock` 里根包那一行 | `release.sh` 同步 |

`packaging/macos/Info.plist` 里那个数字只是占位：你手动 `cp` 出去用会看到它，走打包脚本就一定会被覆盖成真实版本。

## 本地发版

```bash
scripts/release.sh patch                      # 0.1.0 → 0.1.1，跑门禁，然后逐条问要不要提交/打 tag/推送
scripts/release.sh 0.2.0 --commit --tag --push # 全自动，推完就等 CI
scripts/release.sh minor --dry-run             # 只看要改什么
scripts/release.sh 0.2.0-rc.1 --dist           # 门禁里顺带打 universal 的 dmg/zip 到 target/
```

| 选项 | 作用 |
|---|---|
| `--commit` | `chore(release): vX.Y.Z` |
| `--tag` | 注释 tag `vX.Y.Z`（需同时 `--commit`） |
| `--push` | 推分支与 tag，触发 CI 与发版（需同时 `--tag`） |
| `--yes` / `-y` | 等价于上面三个全给 |
| `--no-check` | 跳过本地门禁 |
| `--dist` | 门禁里改成 `bundle-macos.sh --universal --dist`，把 dmg/zip 也打出来 |
| `--allow-dirty` | 工作区不干净时也继续（默认拒绝） |
| `--dry-run` | 不写任何文件 |

门禁与 CI 是同一套：`cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test --no-fail-fast` → `./scripts/bundle-macos.sh`。最后一步不是为了产出安装包，而是它才能发现 build.rs 没生成图标、`PlistBuddy` 改不动 plist、ad-hoc 签名失败这类只在打包时才暴露的问题。

## 流水线

两条工作流，`.github/workflows/` 下两个文件：

**`ci.yml`** — push 到 `main`、PR，以及被 `release.yml` 通过 `workflow_call` 复用。

| job | 跑什么 |
|---|---|
| `check` | `fmt --check` / `clippy -D warnings` / `test --no-fail-fast` |
| `bundle` | `./scripts/bundle-macos.sh`，然后校验 bundle：plist 版本 == Cargo.toml、`--version` 输出、`codesign --verify --strict`、`LSUIElement` 还是 `true` |

`bundle` 单独一个 job，是因为它走 release profile，与 `check` 的 debug 产物不共享；它能验证 `check` 验证不到的东西——图标有没有生成、plist 能不能改、签名会不会失败。`codesign --verify` 那条尤其能挡住「先签名后改 plist」的顺序错误（改了之后签名当场失效）。

**只有 macOS 一个平台。** 这个工具本身就是 macOS 专属的：`espanso` 的 match 目录、LaunchAgent、`NSPanel` 非激活面板、CGEventTap 取键、辅助功能 TCC 授权，没有一样在别的系统上有对应物。在 ubuntu 上跑 `cargo test` 会先卡在 gpui 的图形栈依赖上，换来的信息量为零。

**`release.yml`** — 推 `v*` tag 时触发，`workflow_dispatch` 可手动试跑。

```
ci（复用 ci.yml） → verify（tag 与 Cargo.toml 比对） → build（universal 打包）
```

- `verify`：`v0.2.0` 必须对上 `Cargo.toml` 的 `0.2.0`，不一致直接红（安装包里显示的版本取自后者，对不上就是发错版本）。手动触发时跳过。
- `build`：装 `aarch64-apple-darwin` 与 `x86_64-apple-darwin` 两个 target，跑 `./scripts/bundle-macos.sh --universal --dist`，然后校验：`lipo -archs` 必须同时含 `arm64` 与 `x86_64`、plist 版本等于 tag、`hdiutil verify` 通过、zip 里确实有二进制。
- 产物：`Semisnip-<版本>-universal.dmg` 与 `.zip`，同时挂 workflow artifact（保留 14 天）和 Release。

**Release 是草稿。** 包没有 Apple 开发者签名、也没公证，直接发出去等于让第一个下载的人撞上「无法验证开发者」。草稿留一步：自己下下来在干净机器上装一遍，确认能开，再去 Releases 页面点 Publish。

`workflow_dispatch` 只打包、只上传 artifact，不建 Release——想先看看流水线通不通就用它，不会留下一个空 release。

## 下载的人会遇到什么

未签名 + 未公证 = Gatekeeper 拦截。三种放行方式（Release 正文里也写了）：

```bash
xattr -cr /Applications/Semisnip.app   # 或者右键 →「打开」→ 再点「打开」
```

装完还要手动做两步：辅助功能里勾上 `Semisnip`、`espanso restart`。见 [安装](install.md)。

想彻底去掉这个门槛，得有 Apple Developer 的 Developer ID 证书 + notarytool 公证；那是另一笔开销，当前项目没有做。

## 出问题了看这里

| 现象 | 原因 |
|---|---|
| `tag v0.2.0 与 Cargo.toml 的 version 0.1.0 不一致` | 直接用 `git tag` 打了 tag，没走 `release.sh`。改 Cargo.toml 后重打 tag |
| `error: failed to compile ... target may not be installed` | 本地/CI 缺 `x86_64-apple-darwin`：`rustup target add aarch64-apple-darwin x86_64-apple-darwin` |
| `hdiutil: create failed - Resource busy` | 有残留挂载点：`hdiutil detach /Volumes/Semisnip` 后重跑 |
| Release 里没有资产 | `fail_on_unmatched_files: true` 会让这个变红，别忽略它——通常是 `--dist` 没生效，dmg/zip 没生成 |
| 装完双击没反应 | 先看 `~/Library/Application Support/semisnip/semisnip.log`，再看 [故障排查](troubleshooting.md) |
