# 命令行

`semisnip` 不带参数时按**谁把它拉起来的**分工，两种来源的观感完全不同：

- **终端里裸敲**：只打印帮助。daemon 是常驻进程，会抢键盘、要授权，不该是一条裸命令的默认后果；它由 LaunchAgent 管。
- **Finder 双击 `Semisnip.app`（或 `open -a Semisnip`）**：这就是"我要用这个 app"，于是起 daemon；已经有实例在跑就把片段面板叫出来。判断依据是 LaunchServices 注入的 `__CFBundleIdentifier` 是不是 `com.semisnip.app`（终端里跑继承的是终端自己的 id），和参数无关——两种情况下参数都是空的。

```
semisnip                            打印帮助（不会启动任何东西）
semisnip daemon                     前台启动 daemon（调试用）
semisnip trigger [--origin O]       请求打开面板（origin: tap | espanso | manual）
semisnip reload                     重建索引
semisnip install                    写入 espanso 触发定义并打印后续步骤
semisnip match [子命令]              看/改写进 espanso 的 semisnip.yml
semisnip uninstall [--yes]          卸载（默认只演练，--purge 连配置一起删）
semisnip completion [子命令]         生成/安装 shell 补全
semisnip config [子命令]             配置的查看与修改
semisnip log [选项]                  看日志
semisnip doctor                     体检
semisnip type <text>                调试：合成字符输入
semisnip key <esc|enter|up|down>    调试：合成命名按键
```

命令树用 clap 定义，`--help` / `-V` / 拼错时的「你是不是想输 xxx」都是自动的；`type` / `key` 是隐藏的调试命令，不出现在帮助里。

两条容易踩的输出约定：

- `trigger` 被 espanso 的 shell 变量同步调用，因此**成功时 stdout 必须为空**——espanso 会把 stdout 原样注入文档，多一个字节就是污染。所有错误都走 stderr。
- `type` / `key` 会向系统合成真实键盘事件，仅用于没有辅助功能权限时的冒烟测试。

## `match`

semisnip 只往 espanso 的 `match/` 里写**一个**文件：`semisnip.yml`。同目录下其它的 YAML 都是你的片段，任何命令都不碰。

```bash
semisnip match                       # 打印内容（默认动作，原样输出，方便管道）
semisnip match path                  # 打印路径
semisnip match edit                  # $EDITOR 打开，退出后自动校验
semisnip match open                  # 用系统默认程序打开
semisnip match open --reveal         # 在访达里选中它（不打开）
semisnip match regenerate            # 按当前 exe 路径与 trigger 重写（= install 的写文件那一步）
```

`edit` 退出后只查「能不能被 espanso 用起来」这一层，不试图替代 espanso 的解析器：

- YAML 能否解析、`matches` 是否非空（硬错误，退出码 1）
- `trigger` 是否与 `config.trigger` 一致——tap 路径按配置里的 trigger 吞键，两边不一致时降级触发永远对不上
- `replace` 里有没有把触发串注回文档（旧格式）——注回会和面板的回删/垫空格抢光标，跑 `match regenerate` 换成空输出格式
- shell 变量的 `cmd` 有没有把 stdout 重定向到 `/dev/null`——漏了它，命令输出会被原样注入用户文档

后三条是警告，不拦着你保存。这个文件是**生成物**：`install` / `regenerate` 都会整份重写，手改只适合临时调试；要长期改触发串请用 `semisnip config set trigger`。

## `completion`

子命令有十来个，靠记不现实，所以补全被当成主路径的一部分：

```bash
semisnip completion                 # 按 $SHELL 装好（写脚本 + 接进启动文件）
semisnip completion --dry-run       # 只说明将要改什么
semisnip completion zsh             # 只把脚本打到 stdout，给 eval 用
semisnip completion install --shell fish
semisnip completion uninstall       # 摘干净：删脚本 + 移除启动文件里的接管块
```

装的时候会在启动文件里写一段**带标记**的块，`uninstall` 靠标记原样摘除，不猜哪几行是 semisnip 写的：

```zsh
# >>> semisnip completion >>>
if (( $+functions[compdef] )) && [ -r ".../semisnip/completions/semisnip.zsh" ]; then
  source ".../semisnip/completions/semisnip.zsh"
fi
# <<< semisnip completion <<<
```

各 shell 的落点：

- zsh → `~/.zshrc`（`compdef` 还没就绪时静默跳过，不会每次开终端报错）
- bash → `~/.bash_profile`（存在则用它，其次 `~/.bashrc`）
- fish → `~/.config/fish/completions/semisnip.fish`，fish 自动加载，**不改**任何 rc
- powershell → `$PROFILE`（`~/.config/powershell/Microsoft.PowerShell_profile.ps1`）
- elvish 没有约定俗成的自动加载点，只支持 `semisnip completion elvish` 自己 eval

重复执行是幂等的：块就地替换，不会越装越长。

## `config`

见 [配置](configuration.md#怎么改)。配置文件是 YAML，`semisnip config schema` 打印配套的 JSON Schema 路径（编辑器据此补全与校验）。

## `log`

```bash
semisnip log                         # 最近 40 行（默认 40，-n 改）
semisnip log -n 200                  # 最近 200 行
semisnip log -f                      # 跟随，Ctrl-C 退出
semisnip log -g 触发                  # 只看包含关键词的行（--grep）
semisnip log -l warn                 # 只看 warn 及以上（info/warn/error）
semisnip log path                    # 日志路径
semisnip log clear                   # 清空
```

daemon 以 `.app` 启动时没有终端，日志文件是唯一的回溯手段。`--level` 读的是 `env_logger` 那行 `[时间 LEVEL target]` 里的级别；没有前缀的续行（栈回溯）一律放行，不会漏掉上下文。

## `uninstall`

默认**只演练**，把每一项和路径都列出来，加 `--yes` 才真删：

```bash
semisnip uninstall                   # 演练
semisnip uninstall --yes             # 真删（保留配置与日志）
semisnip uninstall --yes --purge     # 连配置、使用频率、日志一起删
semisnip uninstall --yes --keep-app  # 只拆 LaunchAgent 与 espanso 定义，留 app
```

顺序是先 `launchctl bootout` 再删 app——launchd 的 `ProgramArguments` 指向 app 内部，反过来会触发一次失败重启。**任何情况下都不碰 espanso 的片段 YAML**，只删自己生成的那一个 `match/semisnip.yml`。完成后会提醒你 `espanso restart`。
