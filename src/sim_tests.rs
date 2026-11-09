//! 端到端模拟测试：不依赖 espanso、不需要辅助功能授权，在进程内完整跑通
//! 「打字 → 触发 → 面板 → 确认 → 回传 → 展开」全链路，并断言目标文档的
//! 每一步状态。
//!
//! ## 替身与真实的对应关系
//!
//! | 模拟件 | 替代的真实组件 | 保真度 |
//! |---|---|---|
//! `Doc` | 目标 App 的文本缓冲 | 光标恒在末尾（与真实展开场景一致） |
//! `TriggerMachine`（**真件**） | 同名 | 直接使用生产代码，不复制逻辑 |
//! `Sim` 的路由分支 | `trigger::tap` 的回调 | 只模拟与文档相关的规则：物理键喂状态机、面板吞键、**外部注入不进状态机** |
//! `FakeEspanso` | espanso daemon | 模拟被动匹配（原生 + 降级）、`match exec` 的 TriggerCompensation 与注入、以及"注入时机不可预估"（延迟结算） |
//! `SimBackend`（实现 `handoff::Backend`） | CGEvent 合成 + espanso CLI | 把 confirm 序列的每个动作记进 `injected` 并作用到 `Doc` |
//! 唤醒来源的 `erase` | `ipc::server::dispatch` | 真实里 `Request::Open` 固定带 `erase: 0`（触发串要么没进文档、要么已被 espanso 擦掉），测试同样从 0 起算 |
//!
//! ## 这些测试守护的不变量
//!
//! 1. **文档诚实**：触发串一个键都不吞；取消 = 纯关闭，零合成按键；
//! 2. **确认回删有据**：回删数恰好等于文档里躺着的触发符个数；
//! 3. **占位串不受时序影响**：补偿退格无论何时到达（立即 / 下一轮 / 面板
//!    开着时），删的都是恰好 `trigger_len` 个可丢弃字符——这是"espanso 原生
//!    触发与面板确认交替使用互不干扰"（历史 bug）的回归防线；
//! 4. **外部注入不进状态机**：展开文本里就算有一串 `;` 也不能误触发。
//!
//! 无法模拟的部分（真实系统里存在、但与本链路无关）：gpui 窗口、AX 光标
//! 定位、鼠标监听。面板交互只建模了「吞键进过滤串 / Esc / 回车」三件事。

use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::degraded_trigger;
use crate::config::{Config, Placeholder};
use crate::handoff::{Backend, Outcome, confirm_with};
use crate::index::model::{Body, Hit, Snippet, SourceRef};
use crate::ipc::proto::Origin;
use crate::trigger::keymap;
use crate::trigger::machine::{TriggerAction, TriggerConfig, TriggerMachine};

/// 测试用的触发串。与 `Config::default()` 一致；要改就得连着
/// `FakeEspanso` 的降级匹配一起改（它是从同一个串派生的）。
const TRIGGER: &str = ";;";

// ---------------------------------------------------------------------------
// Doc：目标 App 的文本缓冲替身
// ---------------------------------------------------------------------------

/// 目标应用的文档模型。光标永远在末尾——真实场景里用户刚打完触发串、
/// 面板是非激活的，所有插入（用户按键、合成退格、展开文本）都发生在尾部。
struct Doc {
    chars: Vec<char>,
}

impl Doc {
    fn new() -> Self {
        Self { chars: Vec::new() }
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn type_char(&mut self, c: char) {
        self.chars.push(c);
    }

    fn type_str(&mut self, s: &str) {
        self.chars.extend(s.chars());
    }

    /// 退格 `n` 次。文档不够长时删到空为止（真实 App 也是这样）。
    fn backspace(&mut self, n: usize) {
        let keep = self.chars.len().saturating_sub(n);
        self.chars.truncate(keep);
    }

    fn ends_with(&self, suffix: &str) -> bool {
        self.text().ends_with(suffix)
    }
}

// ---------------------------------------------------------------------------
// FakeEspanso：espanso daemon 替身
// ---------------------------------------------------------------------------

/// espanso 替身。两套职责：
///
/// 1. **被动匹配**（`observe`）：真实 espanso 监听键盘、维护自己的缓冲；
///    命中 trigger 时按全长回删再注入 replace。这里简化成"扫描 Doc 尾部"，
///    净效果与真实一致（回删 + 注入），且**绕过 semisnip 的状态机**——
///    模拟件天然满足"外部注入不进状态机"。
/// 2. **match exec**（`exec_deferred`）：TriggerCompensation 按 trigger 全长
///    回删 + 注入展开文本。注入到达时机不可预估（shell 变量一跑几百毫秒），
///    所以推进 `pending`，由测试决定何时 `settle`——这正是占位串方案的靶心。
struct FakeEspanso {
    /// daemon 挂掉：被动匹配不动作、`match exec` 失败（IPC 都不通）。
    dead: bool,
    /// 原生匹配表：trigger → replace（已求值，不含变量）。
    native: Vec<(String, String)>,
    /// 片段匹配表（`match exec -t` 用）：trigger → 展开文本（已求值）。
    snippets: Vec<(String, String)>,
    /// 降级匹配的 trigger（`cli::degraded_trigger` 派生，与磁盘 yml 同源）。
    degraded: String,
    /// 尚未到达的注入：(回删数, 注入文本)。settle 时按序作用到 Doc。
    pending: Vec<(usize, String)>,
    /// 降级匹配发出的面板唤醒（真实里经 `semisnip panel` → UDS 到达调度器）。
    opened: Vec<Origin>,
}

impl FakeEspanso {
    fn new() -> Self {
        Self {
            dead: false,
            native: vec![
                (":log".into(), "console.log();".into()),
                (":shell".into(), "sh -l".into()),
            ],
            snippets: vec![("sh".into(), "SH-OK;".into())],
            degraded: degraded_trigger(TRIGGER),
            pending: Vec::new(),
            opened: Vec::new(),
        }
    }

    /// espanso 观察到文档变化（等价于真实里它看到了新按键）。
    ///
    /// 一次只消化一条匹配——真实 espanso 一轮也只展开一个 trigger。
    fn observe(&mut self, doc: &mut Doc) {
        if self.dead {
            return;
        }
        // 原生匹配：命中 → 按 trigger 全长回删 → 注入 replace。
        for (trigger, replace) in &self.native {
            if doc.ends_with(trigger) {
                doc.backspace(trigger.chars().count());
                doc.type_str(replace);
                return;
            }
        }
        // 降级匹配：命中 → 按 trigger 全长回删 → **只擦不注回**（`replace` 取的是
        // 被重定向掉的 stdout，见 `cli::render_match_file`），同时唤醒面板。
        // 注回触发串会和面板的「回删 + 垫占位串」抢光标，历史 bug 就是这么来的。
        if doc.ends_with(&self.degraded) {
            doc.backspace(self.degraded.chars().count());
            self.opened.push(Origin::EspansoShell);
        }
    }

    /// `match exec -t <trigger>` 的替身。不立即生效：把
    /// (补偿回删数 = trigger 全长, 展开文本) 压进 `pending`，由 `settle`
    /// 在任意晚的时刻结算——模拟"注入时机不可预估"。
    fn exec_deferred(&mut self, trigger: &str) -> anyhow::Result<()> {
        if self.dead {
            anyhow::bail!("espanso daemon 不可达（模拟）");
        }
        let (_, text) = self
            .snippets
            .iter()
            .find(|(t, _)| t == trigger)
            .ok_or_else(|| anyhow::anyhow!("espanso 不认识 trigger `{trigger}`（模拟）"))?;
        self.pending.push((trigger.chars().count(), text.clone()));
        Ok(())
    }

    /// 时间流逝：所有在途注入此刻到达。**绕过状态机直接改文档**——
    /// 真实里这些是外部进程合成的事件，tap 一律放行（`trigger::tap`）。
    fn settle(&mut self, doc: &mut Doc) {
        for (n, text) in self.pending.drain(..) {
            doc.backspace(n);
            doc.type_str(&text);
        }
    }
}

// ---------------------------------------------------------------------------
// Sim：把替身和真件接成一条链路
// ---------------------------------------------------------------------------

/// 打开中的面板现场（对应 `AppState` 里与一轮打开相关的字段）。
struct PanelState {
    origin: Origin,
    /// 确认时要回删的触发符个数（调度器算好的最终值）。
    erase: usize,
}

/// 一条 semisnip 合成动作的记录。取消路径用它断言「零合成按键」。
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// 合成退格。
    Backspace(usize),
    /// 合成了占位串（内容按 `Config::placeholder` 生成，见 `handoff::placeholder_text`）。
    Placeholder(String),
    /// 调了 `match exec`。
    Exec(String),
    /// 写了剪贴板。
    Clipboard(String),
    /// 合成了 Cmd+V。
    Paste,
}

/// 全链路模拟器：真状态机 + 真回传序列 + 假文档 / 假 espanso。
struct Sim {
    doc: Doc,
    /// 触发状态机——**生产真件**，与 tap 回调里喂的是同一套代码。
    machine: TriggerMachine,
    /// 辅助功能授权是否可用。不可用时按键不经过状态机（真实里根本没有 tap）。
    tap_ready: bool,
    panel: Option<PanelState>,
    /// 面板过滤串（面板吞键，字符永远到不了文档）。
    query: String,
    espanso: FakeEspanso,
    /// semisnip 合成的全部动作（按序）。
    injected: Vec<Action>,
}

impl Sim {
    fn new(tap_ready: bool) -> Self {
        Self {
            doc: Doc::new(),
            machine: TriggerMachine::new(TriggerConfig::from_text(TRIGGER, 800).unwrap()),
            tap_ready,
            panel: None,
            query: String::new(),
            espanso: FakeEspanso::new(),
            injected: Vec::new(),
        }
    }

    /// 用户敲一个可打印键（一次物理 keyDown）。
    ///
    /// 路由顺序复刻 `trigger::tap` 的回调：
    /// 1. 面板开着 → 物理键归面板（吞掉，不进文档、不进状态机）；
    /// 2. tap 在线 → 喂状态机：`Pass` 键落文档；`Fired` 键**照常落文档**并开面板；
    /// 3. tap 离线 → 没有拦截者，键直接落文档；
    /// 4. espanso 也在看（真实里它与 semisnip 并行监听）。
    fn press(&mut self, ch: char) {
        if self.tap_ready && self.panel.is_some() {
            // 面板吞键：过滤串收下，文档与状态机都看不到。
            self.query.push(ch);
            return;
        }

        if self.tap_ready {
            let code = keymap::keycode_for_char(ch)
                .unwrap_or_else(|| panic!("测试字符 `{ch}` 必须有键码"));
            match self.machine.feed(code, std::time::Instant::now()) {
                TriggerAction::Fired { erase } => {
                    // 触发键照常落文档（不吞键），随后开面板。
                    self.doc.type_char(ch);
                    self.panel = Some(PanelState {
                        origin: Origin::Tap,
                        erase,
                    });
                }
                TriggerAction::Pass => self.doc.type_char(ch),
            }
        } else {
            // 无 tap：没有拦截者，键直接落进文档。
            self.doc.type_char(ch);
        }

        // espanso 与 semisnip 并行监听同一路按键。
        self.espanso.observe(&mut self.doc);
        self.drain_opens();
    }

    /// 连敲一串字符。
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.press(c);
        }
    }

    /// 消化降级匹配发出的唤醒（真实里是 UDS 事件到达调度器）。
    fn drain_opens(&mut self) {
        for origin in self.espanso.opened.drain(..) {
            // 幂等：与调度器一致，面板已开就忽略重复的 open。
            if self.panel.is_some() {
                continue;
            }
            // `erase` 恒为 0，与 `ipc::server::dispatch` 一致：降级路径上触发串
            // 已经被 espanso 擦干净，文档里没有需要回删的字符（tap 路径则由
            // 状态机的 `Fired { erase }` 自带全长）。真实里调度器还会把这事件
            // 压进 `OpenGate` 等 150ms 落定，这里直接开——测试关心的是文档状态，
            // 闸门本身有独立单测。
            self.panel = Some(PanelState { origin, erase: 0 });
        }
    }

    /// 在面板里确认一条候选（等价于打过滤串后按回车）。
    ///
    /// 真实里过滤与选中是 `AppState::on_key` 的事（已有独立单测），这条链路
    /// 的关键在**确认之后的序列**，所以这里直接选定 hit 走 `confirm_with`。
    fn confirm_hit(&mut self, hit: &Hit, config: &Config, fallback: Option<&str>) -> Outcome {
        let panel = self
            .panel
            .take()
            .expect("确认前面板必须打开（tap 与 espanso 两条路都如此）");
        let mut backend = SimBackend {
            doc: &mut self.doc,
            espanso: &mut self.espanso,
            injected: &mut self.injected,
            clipboard: None,
        };
        confirm_with(hit, panel.erase, config, fallback, &mut backend)
    }

    /// Esc 关面板（纯关闭）。断言没有合成任何按键、文档原样。
    fn esc(&mut self) {
        assert!(self.panel.take().is_some(), "Esc 前面板必须打开");
        let injected = &self.injected;
        assert!(
            injected.is_empty(),
            "取消是纯关闭，不能合成任何按键，实际合成了 {injected:?}"
        );
    }

    /// 模拟时间流逝：在途的外部注入此刻到达。
    fn settle(&mut self) {
        self.espanso.settle(&mut self.doc);
    }

    fn text(&self) -> String {
        self.doc.text()
    }
}

/// [`Backend`] 的测试实现：把 confirm 序列的每个动作记下来并作用到 `Doc`。
///
/// `exec_match` 转给 `FakeEspanso::exec_deferred`（延迟结算）；其余动作立即
/// 生效——它们本来就是 semisnip 自己合成的事件，到达目标是同步的。
struct SimBackend<'a> {
    doc: &'a mut Doc,
    espanso: &'a mut FakeEspanso,
    injected: &'a mut Vec<Action>,
    /// `set_clipboard` 暂存的文本；`paste`（Cmd+V）时落进文档——
    /// 与真实粘贴一致，粘贴插入的就是剪贴板内容。
    clipboard: Option<String>,
}

impl Backend for SimBackend<'_> {
    fn backspace(&mut self, count: usize) {
        self.doc.backspace(count);
        self.injected.push(Action::Backspace(count));
    }

    fn type_placeholder(&mut self, text: &str) {
        for c in text.chars() {
            self.doc.type_char(c);
        }
        self.injected.push(Action::Placeholder(text.to_string()));
    }

    fn exec_match(&mut self, trigger: &str) -> anyhow::Result<()> {
        self.injected.push(Action::Exec(trigger.to_string()));
        self.espanso.exec_deferred(trigger)
    }

    fn set_clipboard(&mut self, text: &str) -> anyhow::Result<()> {
        self.injected.push(Action::Clipboard(text.to_string()));
        self.clipboard = Some(text.to_string());
        Ok(())
    }

    fn paste(&mut self) {
        // 真实 Cmd+V 会把剪贴板内容插进文档；没剪贴板就当粘贴了空。
        let text = self.clipboard.take().unwrap_or_default();
        self.doc.type_str(&text);
        self.injected.push(Action::Paste);
    }
}

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

/// 构造一条片段（照搬 `search::score` 测试里的既有模式）。
fn snippet(trigger: &str, label: &str, body: &str) -> Arc<Snippet> {
    Arc::new(Snippet {
        id: 0,
        triggers: vec![trigger.to_string()],
        label: label.to_string(),
        body: Body::Text(body.to_string()),
        vars: vec![],
        form_fields: vec![],
        search_terms: vec![],
        source: SourceRef {
            file: PathBuf::from("base.yml"),
            ordinal: 0,
            package: None,
        },
        word: false,
        usable: true,
    })
}

/// 面板候选：trigger `sh`，espanso 表里有对应展开。
fn hit_sh() -> Hit {
    Hit {
        snippet: snippet("sh", "uptime", "SH-OK;"),
        trigger: "sh".to_string(),
        score: 0,
        matched: Default::default(),
    }
}

/// 测试配置：关掉一切真实等待（`post_exec_delay_ms`）与路径副作用。
///
/// 占位符固定成 `space`：这些用例走的是「面板确认 → `match exec`」主路径，
/// 而 `Config::default()` 的默认值是 `trigger`（直接委派给 espanso），
/// 那是另一个分支（见 `trigger_placeholder_delegates_instead_of_calling_match_exec`）。
fn config(allow_paste_fallback: bool) -> Config {
    Config {
        post_exec_delay_ms: 0,
        allow_paste_fallback,
        placeholder: Placeholder::Space,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// 场景
// ---------------------------------------------------------------------------

/// tap 在线的完整正路：`;;` 触发 → 确认 → 占位串 → 展开。
///
/// 逐步断言文档：触发串留在文档里 → 确认回删后垫上空格 → 结算后拿到
/// 干净的展开文本。这是整条链路的基准场景。
#[test]
fn tap_path_full_flow_expands_cleanly() {
    let mut sim = Sim::new(true);
    sim.type_text("hello ");
    assert_eq!(sim.text(), "hello ");

    // `;;` 两个键都落进文档（不吞键），面板带着 erase = 全长打开。
    sim.type_text(";;");
    assert_eq!(sim.text(), "hello ;;", "触发串一个键都不吞");
    assert!(sim.panel.is_some(), "命中后面板必须打开");

    // 确认：回删 2 个触发符 → 垫 2 个占位串 → match exec。
    let outcome = sim.confirm_hit(&hit_sh(), &config(true), None);
    assert_eq!(outcome, Outcome::Expanded);
    assert_eq!(
        sim.text(),
        "hello   ",
        "回删后光标前只剩原文与占位串（慢展开期间可见空格是已知限制）"
    );
    assert_eq!(
        sim.injected,
        vec![
            Action::Backspace(2),
            Action::Placeholder("  ".into()),
            Action::Exec("sh".into()),
        ],
        "confirm 序列必须严格是：回删残留 → 垫占位串 → match exec"
    );

    // 补偿退格到达：删掉恰好那 2 个空格，注入展开文本。
    sim.settle();
    assert_eq!(sim.text(), "hello SH-OK;", "结算后文档必须干净、无空格残留");
}

/// 换占位字符不改变链路：零宽占位同样被补偿退格精确收走。
///
/// 这条守的是「占位符可配置」的契约：`placeholder` 只决定**垫什么字符**，
/// 长度与顺序都不能变——espanso 的退格数是由片段 trigger 长度决定的，与内容无关。
#[test]
fn zero_width_placeholder_is_consumed_the_same_way() {
    let mut sim = Sim::new(true);
    sim.type_text("hi ;;");
    assert!(sim.panel.is_some());

    let config = Config {
        placeholder: Placeholder::ZeroWidth,
        ..config(true)
    };
    assert_eq!(sim.confirm_hit(&hit_sh(), &config, None), Outcome::Expanded);

    // 确认当刻：零宽字符看不见，但确实在文档里，且个数等于触发串长度。
    assert_eq!(sim.text(), "hi \u{200b}\u{200b}", "占位串长度必须精确");
    assert_eq!(
        sim.injected,
        vec![
            Action::Backspace(2),
            Action::Placeholder("\u{200b}\u{200b}".into()),
            Action::Exec("sh".into()),
        ]
    );

    sim.settle();
    assert_eq!(sim.text(), "hi SH-OK;", "补偿退格必须把零宽字符也删干净");
}

/// `placeholder = trigger`：把触发串打出去，**不**调 `match exec`。
///
/// 这条守的是"不能双重展开"：espanso 只跳过自己注入的事件，我们打出去的触发串
/// 会被它当成真实输入再命中一次；如果此时还调 `match exec`，同一段文字会展开
/// 两遍。sim 里没有 espanso 的被动匹配，所以断言落在"没有 Exec 这一步"上。
#[test]
fn trigger_placeholder_delegates_instead_of_calling_match_exec() {
    let mut sim = Sim::new(true);
    sim.type_text("hello ;;");
    assert!(sim.panel.is_some());

    let config = Config {
        placeholder: Placeholder::Trigger,
        ..config(true)
    };
    let outcome = sim.confirm_hit(&hit_sh(), &config, None);
    assert_eq!(outcome, Outcome::Delegated, "这条路交给 espanso 自己命中");

    assert_eq!(sim.text(), "hello sh", "触发串必须原样打进文档");
    assert_eq!(
        sim.injected,
        vec![Action::Backspace(2), Action::Placeholder("sh".into()),],
        "只回删面板触发串 + 打出片段触发串；绝不能出现 Exec"
    );
}

/// 取消是纯关闭：文档保持诚实，零合成按键，状态机无残留。
#[test]
fn cancel_pure_close_leaves_document_honest() {
    let mut sim = Sim::new(true);
    sim.type_text("say ;;");
    assert!(sim.panel.is_some());

    sim.esc();
    assert_eq!(sim.text(), "say ;;", "取消后文档里就是用户打的原文");
    assert_eq!(sim.query, "", "过滤串随面板一起清空");

    // 关键回归：取消后状态机必须已复位——再打一个 `;` 是新序列的起点，
    // 不能接着旧计数把 `;;;` 误判成触发。
    sim.press(';');
    assert_eq!(sim.machine.armed_len(), 1, "单个 `;` 是新序列起点，不触发");

    // 但这一按同时把文档凑成了 `say ;;;`，espanso 的降级匹配随即命中：它按
    // trigger 全长回删，三个分号一起没了，面板又被唤醒一次。这正是文档里记的
    // 已知限制——降级匹配不区分"tap 是否在线"，而 semisnip 从没见过这段文本，
    // 补不回来（`docs/troubleshooting.md#已知限制`）。
    assert_eq!(sim.text(), "say ", "降级匹配命中的代价：`;;;` 被整段擦掉");
    let panel = sim.panel.as_ref().expect("降级匹配会再唤醒一次面板");
    assert_eq!(panel.origin, Origin::EspansoShell);
    assert_eq!(panel.erase, 0, "触发串已被 espanso 擦掉，没有要回删的字符");
}

/// espanso 原生触发与面板确认**交替使用**（历史 bug 场景）：
/// 前一轮的展开还没到达时，下一轮的触发、面板、确认照常进行，
/// 两轮的注入最终都精确落位。
#[test]
fn native_espanso_and_panel_interleave_without_interference() {
    let mut sim = Sim::new(true);

    // 第 1 轮：espanso 原生 `:log`（不经 semisnip 面板）。
    sim.type_text("a ");
    sim.type_text(":log");
    assert_eq!(sim.text(), "a console.log();", "原生触发即时展开");

    // 第 2 轮：面板确认 `sh`，但展开慢（shell 变量）——结算前文档里只有空格。
    //
    // 先补一个空格：`console.log();` 以 `;` 结尾，紧接着打 `;;` 会让文档凑出
    // `;;;`，espanso 的降级匹配当场命中、把展开末尾的分号一起擦掉。那是另一条
    // 缺陷（见 `known_defect_degraded_match_eats_trailing_semicolon`），本用例
    // 只想验证"两轮注入互不干扰"，所以先把它绕开。
    sim.type_text(" ");
    sim.type_text(";;");
    let outcome = sim.confirm_hit(&hit_sh(), &config(true), None);
    assert_eq!(outcome, Outcome::Expanded);
    assert_eq!(sim.text(), "a console.log();   ");

    // 慢展开尚未到达时，用户继续打第 3 轮的原生触发 `:shell`。
    // 先结算：补偿退格删的是占位串，不是 `:shell` 的任何字符——
    // 这就是占位串方案对"时序不可预估"的全部依赖。
    sim.settle();
    assert_eq!(sim.text(), "a console.log(); SH-OK;");
    // 外部注入（含文本里的 `;`）不进状态机：机器没有半截触发序列。
    assert_eq!(sim.machine.armed_len(), 0, "展开文本不能污染触发状态机");

    sim.type_text(":shell");
    assert_eq!(
        sim.text(),
        "a console.log(); SH-OK;sh -l",
        "三轮注入（原生 → 面板 → 原生）全部精确落位"
    );
}

/// 无辅助功能授权（tap 离线）的降级正路：`;;;` 被 espanso 擦掉（不注回任何东西）
/// 并唤醒面板；文档里已经没有触发符，所以 `erase = 0`，确认序列只剩「垫占位串 +
/// match exec」——与 tap 路只差一个 `erase`。
#[test]
fn degraded_path_without_accessibility() {
    let mut sim = Sim::new(false);

    sim.type_text("hi ");
    // 多打一个末字符凑齐降级触发串。
    sim.type_text(";;;");
    assert_eq!(
        sim.text(),
        "hi ",
        "降级匹配只擦不注回：触发串必须从文档里消失"
    );
    let panel = sim.panel.as_ref().expect("降级命中必须唤醒面板");
    assert_eq!(panel.origin, Origin::EspansoShell);
    assert_eq!(panel.erase, 0, "触发串已被 espanso 擦掉，没有要回删的字符");

    let outcome = sim.confirm_hit(&hit_sh(), &config(true), None);
    assert_eq!(outcome, Outcome::Expanded);
    // 确认当刻：垫了 trigger_len 个占位串，等 espanso 的补偿退格来收。
    assert_eq!(sim.text(), "hi   ");
    sim.settle();
    assert_eq!(sim.text(), "hi SH-OK;");
    assert_eq!(
        sim.injected,
        vec![Action::Placeholder("  ".into()), Action::Exec("sh".into())],
        "没有触发符可回删，序列从垫占位串开始"
    );
}

/// **已知缺陷的现场记录，不是期望行为。**
///
/// tap 在线时降级匹配照样能命中：espanso 看的是**按键流**，与 tap 拦不拦无关。
/// 只要按 `;;` 之前文档正好以 `;` 结尾（典型场景：刚展开的片段以 `;` 收尾，
/// 而且末尾那个 `;` 前不是 `;` 所以状态机没被提前触发），第二个 `;` 落下的瞬间
/// 文档里就是 `;;;`——espanso 按 trigger 全长回删三个字符，**连用户正文里的
/// 那个分号一起删掉**，面板则照常由 tap 打开（`erase` 仍按 2 算），最终确认时
/// 又多删两个字符。
///
/// 修法方向：让 CLI 在 tap 可用（daemon `status` 为真）时把触发串原样交回
/// `replace`（净零，且不再开面板）。难点是那段交回的注入会与面板的垫占位串共用
/// 光标——正是 `cli::render_match_file` 注释里记着的老坑，得连时序一起设计。
/// 修好后这条测试要改成断言文档完全不动。
#[test]
fn known_defect_degraded_match_eats_trailing_semicolon() {
    let mut sim = Sim::new(true);

    // 原生展开成 `console.log();`：末尾是 `;`，且它前面的 `)` 让状态机归零。
    sim.type_text("a ");
    sim.type_text(":log");
    assert_eq!(sim.text(), "a console.log();");
    assert_eq!(sim.machine.armed_len(), 0);

    sim.type_text(";;");
    assert_eq!(
        sim.text(),
        "a console.log()",
        "缺陷现场：降级匹配把展开末尾的分号也擦了"
    );
    let panel = sim.panel.as_ref().expect("tap 路径照常开面板");
    assert_eq!(panel.origin, Origin::Tap, "面板来源仍是 tap 命中");
    assert_eq!(panel.erase, 2, "tap 路径的 erase 仍是触发串全长");
}

/// 面板开着时，上一轮未到达的注入（补偿退格 + 展开文本）必须原样落到文档：
/// 面板不能吞它们、也不能把它们当成过滤串——历史 bug 的另一半。
#[test]
fn external_injection_passes_through_while_panel_open() {
    let mut sim = Sim::new(true);

    // 制造一轮"在途注入"：原生触发放进 pending（模拟注入管线压着慢展开）。
    sim.type_text("x ");
    // 直接预置在途注入，模拟"上一轮 match exec 的展开此刻才到"。
    sim.espanso.pending.push((0, "[晚到]".to_string()));

    // 注入到达前面板已开：`;;` 正常触发。
    sim.type_text(";;");
    assert!(sim.panel.is_some());
    assert_eq!(sim.query, "");

    // 慢注入在面板打开期间到达：必须直接进文档。
    sim.settle();
    assert_eq!(sim.text(), "x ;;[晚到]", "外部注入穿透面板落进文档");
    assert_eq!(sim.query, "", "注入文本绝不能被当成面板过滤串");
    assert!(sim.panel.is_some(), "面板保持打开");

    // 之后 Esc：纯关闭，文档连同晚到的展开一起保住。
    sim.esc();
    assert_eq!(sim.text(), "x ;;[晚到]");
}

/// espanso daemon 挂掉：exec 失败后必须先自己收走占位串，再走（或不走）
/// 粘贴兜底。三种兜底配置逐一断言文档终态。
#[test]
fn exec_failure_reclaims_pads_and_falls_back() {
    // ① 兜底关闭：触发串已删、空格已收、无展开——Failed。
    let mut sim = Sim::new(true);
    sim.espanso.dead = true;
    sim.type_text("hi ;;");
    let outcome = sim.confirm_hit(&hit_sh(), &config(false), None);
    assert!(matches!(outcome, Outcome::Failed(_)));
    assert_eq!(sim.text(), "hi ", "失败路径不留触发符也不留占位串");
    assert_eq!(
        sim.injected,
        vec![
            Action::Backspace(2),
            Action::Placeholder("  ".into()),
            Action::Exec("sh".into()),
            Action::Backspace(2),
        ],
        "exec 失败后必须立即补一次等长回删收走空格"
    );

    // ② 兜底开启且有兜底文本：粘贴展开，文档 = 原文 + 兜底文本。
    let mut sim = Sim::new(true);
    sim.espanso.dead = true;
    sim.type_text("hi ;;");
    let outcome = sim.confirm_hit(&hit_sh(), &config(true), Some("FALLBACK"));
    assert_eq!(outcome, Outcome::Pasted);
    assert_eq!(sim.text(), "hi FALLBACK");
    assert_eq!(
        sim.injected,
        vec![
            Action::Backspace(2),
            Action::Placeholder("  ".into()),
            Action::Exec("sh".into()),
            Action::Backspace(2),
            Action::Clipboard("FALLBACK".into()),
            Action::Paste,
        ],
        "粘贴兜底 = 先收占位串，再剪贴板 + Cmd+V"
    );

    // ③ 兜底开启但无兜底文本（空片段）：同样 Failed，文档干净。
    let mut sim = Sim::new(true);
    sim.espanso.dead = true;
    sim.type_text("hi ;;");
    let outcome = sim.confirm_hit(&hit_sh(), &config(true), None);
    assert!(matches!(outcome, Outcome::Failed(_)));
    assert_eq!(sim.text(), "hi ");
}
