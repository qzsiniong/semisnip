//! UI 侧调度：消费 tap 线程与 IPC 线程的事件，驱动窗口与状态。
//!
//! 轮询（16ms 忙 / 50ms 闲）而不是跨线程唤醒——gpui 的 `App` 只能在 UI
//! 线程访问，轮询是这里最稳的桥。这和 snippets / clipbeam 的做法一致。

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use gpui_kit::{App, AsyncApp, Entity};

use crate::app::event::Event;
use crate::app::state::{self, AppState, Intent};
use crate::ipc::proto::Origin;
use crate::trigger::tap;
use crate::ui;

/// 降级唤醒的"落定"等待（见 [`OpenGate`]）。
///
/// 150ms 的来源是 espanso 的执行顺序，不是拍脑袋：命中被动匹配后它先跑 shell
/// 变量（就是我们那个 CLI），变量退出才发 TriggerCompensation（回删触发串）。
/// 我们这边收到 IPC 响应就开窗，可能比那串退格更早。等这 150ms 是在等
/// "espanso 把触发串擦干净"，之后面板才允许出现——用户按得再快也不会和它抢光标。
/// 改小：退格可能还没落，回删会吃掉文档里的真实字符。改大：面板出现变慢。
const ESPANSO_SETTLE: Duration = Duration::from_millis(150);

/// 降级唤醒的「稍后再开」闸门。
///
/// 为什么需要它：降级路径的文档状态不是"面板一出现就定型"的。espanso 命中
/// `;;;` 之后要等 shell 变量退出才开始回删触发串，而面板只等 IPC 响应——两者
/// 之间有个几毫秒到几百毫秒的空窗。用户在空窗里按回车，`confirm` 的回删与
/// 垫空格就会和 espanso 的退格落在同一个光标位置，表现为"删多了/删少了"。
///
/// 所以 `Origin::EspansoShell` 的 open 一律先压在这里，到点再开窗。闸门只有
/// 一个槽：重复的降级唤醒是幂等的（espanso 只会有一条匹配命中）。
///
/// **它只存 `(origin, erase)`，不存 `Event`，这是刻意的**：闸门放行出来的东西
/// 必须直接进 `open_panel`。曾经它返回 `Event`，调用方顺手把事件又交给
/// [`handle`]，而 `handle` 看到 `EspansoShell` 就再压回闸门——于是每 150ms
/// 自己推迟自己一次，面板永远不出现（日志里没有"打开面板"，也没有报错）。
/// 现在这个循环在类型层面就写不出来。
struct OpenGate {
    /// 待开的 open（origin, erase）与它的到期时刻。
    pending: Option<(Instant, Origin, usize)>,
}

impl OpenGate {
    fn new() -> Self {
        Self { pending: None }
    }

    /// 压下一条待开的唤醒；已有待开时**保留先到的**（重复 open 不该把开窗时间
    /// 一直往后推）。
    fn defer(&mut self, origin: Origin, erase: usize, now: Instant) {
        if self.pending.is_none() {
            self.pending = Some((now + ESPANSO_SETTLE, origin, erase));
        }
    }

    /// 到点了就取走待开的参数，否则返回 `None`。用显式 `now` 而不是内部读时钟，
    /// 是为了让闸门本身可以被单测（见文件末尾）。
    fn take_due(&mut self, now: Instant) -> Option<(Origin, usize)> {
        match &self.pending {
            Some((due, _, _)) if now >= *due => self
                .pending
                .take()
                .map(|(_, origin, erase)| (origin, erase)),
            _ => None,
        }
    }

    /// 丢弃待开事件。别的路径已经把面板打开了、或者进程要退出时必须调，
    /// 否则会"取消之后 150ms 又被降级事件把面板弹出来"。
    fn clear(&mut self) {
        self.pending = None;
    }
}

/// tap 的看门狗状态。见 [`watch_tap`]。
struct TapWatch {
    /// 上次对齐（读 `tap::ready()`）的时间。
    last_check: Instant,
    /// 下一次允许「重新安装」的时间。
    next_retry: Instant,
    /// 当前重试间隔。失败时指数退避（1→2→4→8→10 秒封顶），
    /// 面板正卡在授权页且已授权时压回 1 秒——用户在等，不能让他干等 10 秒。
    retry_delay: Duration,
    /// 连续几次读到「已就绪但系统说未授权」。`AXIsProcessTrusted` 存在缓存，
    /// 单次 false 不足以判定撤销；连续 3 次（约 3 秒）才真正回退。
    untrusted_streak: u32,
}

impl TapWatch {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            last_check: now,
            next_retry: now,
            retry_delay: Duration::from_secs(1),
            untrusted_streak: 0,
        }
    }
}

/// 下一次重试间隔。`hurry` 时固定 1 秒，否则指数退避、10 秒封顶。
fn next_retry_delay(current: Duration, hurry: bool) -> Duration {
    if hurry {
        Duration::from_secs(1)
    } else {
        (current * 2).min(Duration::from_secs(10))
    }
}

/// 启动事件循环（协程形式，跑在 gpui 的主线程上）。
///
/// 循环本身没有"等待事件"的阻塞调用：每轮先跑一次 tap 看门狗，再按面板开合
/// 决定下一次醒来的间隔（开着 16ms≈60fps，关着 50ms 省电），然后 `try_recv`
/// 把这一轮积累的事件全部处理掉。
///
/// 为什么用轮询而不是阻塞 `recv`：`std::sync::mpsc` 的接收端没法塞进 gpui 的
/// async 执行器（没有 async 版本），而 16/50ms 的空转代价在这样一个每秒只处理
/// 几次事件的进程里可以忽略。**代价必须知道**：这个循环永远不会退出，
/// 面板关闭时它仍在跑——所以里面不能有"只在打开时该做"的重活。
pub fn spawn(cx: &mut AsyncApp, state: Entity<AppState>, rx: Receiver<Event>, tx: Sender<Event>) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let bg = cx.background_executor().clone();
        let mut watch = TapWatch::new();
        let mut gate = OpenGate::new();
        loop {
            let busy = cx.update(|cx: &mut App| {
                watch_tap(cx, &state, &mut watch);
                // 到点的降级唤醒在这里放行，**直接开窗**（闸门只给参数，见
                // `OpenGate`：喂回 `handle` 会被重新压回去，面板就永远不开）。
                // 轮询周期本身就是它的分辨率（闲时 50ms），所以 ESPANSO_SETTLE
                // 再多等一轮是正常的。
                if let Some((origin, erase)) = gate.take_due(Instant::now()) {
                    open_panel(cx, &state, origin, erase);
                }
                state.read(cx).open
            });
            bg.timer(Duration::from_millis(if busy { 16 } else { 50 }))
                .await;

            while let Ok(event) = rx.try_recv() {
                cx.update(|cx: &mut App| handle(cx, &state, &tx, &mut gate, event));
            }
        }
    })
    .detach();
}

/// tap 看门狗：把「有没有键盘接管」这件事与面板状态对齐。
///
/// 两件事只有在这里做才不容易出错：
///
/// 1. **同步 `tap_ok`**。`AppState::new` 里读的那次 `tap::ready()` 早于
///    `tap::install`，好机器上也一定是 false；只有每次开面板和这里各对齐一次，
///    面板画的才是真实状态。
/// 2. **权限补上之后自动接上**。`install()` 只跑一次，用户后来才去系统设置
///    勾授权的话，进程这边不会自己知道——以前必须重启 daemon 才生效。
///    这里按退避间隔试一次（`retry` 内部还会先看权限，没权限时不会建线程）。
fn watch_tap(cx: &mut App, state: &Entity<AppState>, watch: &mut TapWatch) {
    const CHECK: Duration = Duration::from_secs(1);
    /// 撤销判定需要的连续 false 次数（`AXIsProcessTrusted` 有缓存，防误判）。
    const UNTRUSTED_STREAK: u32 = 3;

    if watch.last_check.elapsed() < CHECK {
        return;
    }
    watch.last_check = Instant::now();

    // tap 已就绪但系统说没授权：连续确认 3 次后撤回，避免"授权了却假死"。
    // 刚授权时 `AXIsProcessTrusted` 可能短暂仍为 false，但那时 `ready()` 还是
    // false，不会进这个分支；等到 tap 真装上了，系统状态也已稳定。
    if tap::ready() {
        if tap::accessibility_granted() {
            watch.untrusted_streak = 0;
        } else {
            watch.untrusted_streak += 1;
            if watch.untrusted_streak >= UNTRUSTED_STREAK {
                watch.untrusted_streak = 0;
                tap::mark_unavailable("辅助功能授权已被撤销，键盘接管已停止");
            }
        }
    }

    if !tap::ready() && Instant::now() >= watch.next_retry {
        // 面板正卡在授权页、且系统那边确实已经授权时才用 1 秒快试——
        // 用户在等结果；其余场景按指数退避，避免长期没权限时每 1 秒白建线程。
        let hurry = {
            let state = state.read(cx);
            state.open && !state.tap_ok && tap::accessibility_granted()
        };
        match tap::retry() {
            tap::Retry::Spawned => log::info!("检测到辅助功能已授权，正在重装键盘 tap"),
            tap::Retry::SpawnFailed => log::error!("键盘 tap 线程启动失败，稍后重试"),
            tap::Retry::NotInstalled => log::debug!("tap 尚未初始化，稍后重试"),
            tap::Retry::NotTrusted => log::debug!("等待辅助功能授权，稍后重试"),
            tap::Retry::AlreadyReady | tap::Retry::Disabled => {}
        }
        watch.retry_delay = next_retry_delay(watch.retry_delay, hurry);
        watch.next_retry = Instant::now() + watch.retry_delay;
    }

    let ready = tap::ready();
    if state.read(cx).tap_ok == ready {
        return;
    }
    state.update(cx, |state, cx| {
        state.tap_ok = ready;
        // 授权到位就把「降级中」的警告擦掉，否则面板底部会一直挂着一条过时消息；
        // 授权页的失败原因也一并清掉，下次再出问题由新事件重新填。
        // 同时补推直达词表：首次 `set_direct_triggers` 落在 tap 装好之前，
        // 不补的话统计要等下一次索引重建才生效。
        if ready {
            state.status.clear();
            state.permission_reason.clear();
            state.refresh_direct_triggers();
        }
        cx.notify();
    });
    // 引导窗口的体检只在打开时算一次；tap 状态是外部变化的，这里顺手对齐
    // 它的「辅助功能」一行，用户不用再点一次「重新检查」才看到变绿。
    if let Some(onboarding) = cx
        .try_global::<crate::ui::onboarding::GlobalOnboarding>()
        .map(|global| global.0.clone())
    {
        onboarding.update(cx, |onboarding, cx| {
            onboarding.sync_tap();
            cx.notify();
        });
    }
    log::info!(
        "键盘 tap {}",
        if ready {
            "已接上（面板现在能收按键）"
        } else {
            "不可用（降级为 espanso 触发）"
        }
    );
}

fn handle(
    cx: &mut App,
    state: &Entity<AppState>,
    tx: &Sender<Event>,
    gate: &mut OpenGate,
    event: Event,
) {
    match event {
        Event::Open { origin, erase } => {
            // 降级唤醒：先在闸门里等 espanso 把触发串擦干净（见 `OpenGate`）。
            // 这条路径上 `erase` 恒为 0——espanso 已经把文档里的触发串删掉了，
            // 我们只需在干净的光标位置垫占位空格（见 `cli::render_match_file`）。
            //
            // 注意：压进闸门的事件由 `spawn` 里的 `take_due` 直接交给
            // `open_panel`，**不要**再回到这里，否则会被重新压回去、永远不开。
            if origin == Origin::EspansoShell {
                gate.defer(origin, erase, Instant::now());
                // 这条日志是排障的锚点：降级路径失败时，"有没有收到唤醒"和
                // "收到后为什么没开窗"是两种完全不同的故障，没有它只能靠猜。
                log::info!("降级唤醒已排队：{ESPANSO_SETTLE:?} 后开窗（等 espanso 擦掉触发串）");
                return;
            }
            // 别的路径要开面板了：必须先丢掉待开的降级事件，否则"取消之后
            // 150ms 面板又被它弹出来"。
            gate.clear();
            open_panel(cx, state, origin, erase);
        }

        Event::Key(msg) => {
            let intent = state.update(cx, |state, cx| {
                let intent = state.on_key(msg);
                cx.notify();
                intent
            });
            match intent {
                Intent::None => {}
                Intent::Cancel => cancel(cx, state),
                Intent::Confirm(hit) => confirm(cx, state, hit),
                Intent::Reload => {
                    let index = state.read(cx).index.clone();
                    index.reload_async(tx.clone());
                }
                Intent::OpenSettings => {
                    // 先关面板：设置窗口是激活窗口，两者叠在一起会让用户
                    // 分不清键盘归谁。
                    cancel(cx, state);
                    if let Err(err) = ui::settings::open(cx) {
                        log::error!("打开偏好设置失败：{err}");
                    }
                }
            }
        }

        Event::Cancel => {
            gate.clear();
            cancel(cx, state);
        }
        Event::ClickOutside => {
            gate.clear();
            cancel(cx, state);
        }

        Event::IndexUpdated => {
            state.update(cx, |state, cx| {
                state.sync_index();
                cx.notify();
            });
        }

        // 设置窗口/菜单栏落盘之后投过来的：把新配置换进面板，并让触发状态机跟着换。
        // 面板没开时也要处理——trigger 是全局的，下一次 `;;` 就得按新键码走。
        Event::ConfigChanged => {
            let was_open = state.read(cx).open;
            state.update(cx, |state, cx| {
                state.reload_config();
                cx.notify();
            });
            // 暂停的瞬间面板还开着的话立刻收起来：暂停的语义是「不再接管键盘」，
            // 留一个开着的面板等于继续吞键，用户会以为开关没生效。
            if !state.read(cx).config.enabled && was_open {
                cancel(cx, state);
            }
            // 菜单文案与图标跟着 enabled 换。菜单栏是唯一不打开窗口就能看到
            // 状态的地方，慢一拍都会让用户以为点了没反应。
            crate::tray::sync(cx);
        }

        Event::Reload => {
            let index = state.read(cx).index.clone();
            index.reload_async(tx.clone());
        }

        // 面板外直接打出了已知的直达触发词（tap 观察输入得到，属于估算）。
        // 不刷新 UI：面板多半没开，通知只会让 gpui 去找一个不存在的窗口。
        Event::DirectUse(trigger) => {
            state.update(cx, |state, _| state.record_direct_use(&trigger));
        }

        // 轻提示到点自动收窗。窗口可能已经被下一次展开顶掉，关闭是幂等的。
        Event::HintClose => ui::toast::close(cx),

        Event::TapUnavailable(message) => {
            log::warn!("{message}");
            state.update(cx, |state, cx| {
                state.tap_ok = false;
                // 授权页不画底部状态条，技术化原因必须进 `permission_reason`。
                state.permission_reason = message;
                cx.notify();
            });
        }

        Event::ConfirmIndex(index) => {
            gate.clear();
            let hit = state.read(cx).hits.get(index).cloned();
            if let Some(hit) = hit {
                confirm(cx, state, hit);
            }
        }

        Event::Shutdown => {
            log::info!("收到退出请求");
            gate.clear();
            cx.quit();
        }
    }
}

/// 真正把面板画出来（从 [`handle`] 的 `Event::Open` 分支里抽出来的）。
///
/// `erase` 的来源只有三种，且都不再靠猜：tap 事件自带触发串全长；手动 / 菜单栏
/// 为 0；降级路径为 0（触发串已被 espanso 擦掉，见 `cli::render_match_file`）。
fn open_panel(cx: &mut App, state: &Entity<AppState>, origin: Origin, erase: usize) {
    // 幂等：espanso 的降级唤醒与自己的 tap 可能同时到达（日志里见过两次
    // open 相隔几秒的现场）。先到的那个已经把 erase 记下了，后到的忽略。
    if state.read(cx).open {
        log::debug!("面板已打开，忽略重复的 open");
        return;
    }
    log::info!("打开面板：origin={origin:?} erase={erase}");
    state.update(cx, |state, cx| {
        state.begin_open(origin, erase);
        // 开面板这一刻按 tap 的真实状态决定画什么：有授权就画列表，
        // 没有就画授权页——摆一列点了没反应的候选等于骗人。
        state.tap_ok = tap::ready();
        if state.tap_ok {
            state.permission_reason.clear();
        }
        cx.notify();
    });
    tap::set_panel_open(true);
    if let Err(err) = ui::window::open(cx, state.clone()) {
        log::error!("打开面板失败：{err}");
        tap::set_panel_open(false);
        state.update(cx, |state, cx| {
            state.begin_close();
            state.status = format!("面板打开失败：{err}");
            cx.notify();
        });
    }
}

/// 取消：收起面板。**纯关闭，一个合成按键都不发。**
///
/// 取消的语义是「当作没触发过」。tap 路径上这条字面成立：触发串的所有键都没有
/// 被吞（见 `trigger::machine`），文档里本来就是用户打的 `;;`，关掉面板即还原。
/// 早期版本要在这里把被吞的触发键"补回"文档（合成按键 + 触发静默窗口防死循环），
/// 不吞键之后这套机制整体消失了。
///
/// 降级路径（`Origin::EspansoShell`）**不再如此**：`;;;` 在你按下第三个键的那一刻
/// 就被 espanso 擦掉了（它的 TriggerCompensation），semisnip 从没见过它，也就
/// 补不出来。这是刻意的取舍——把触发串注回文档的代价是它与面板的回删/垫空格
/// 抢光标（见 `cli::render_match_file`）。tap 路径没有这个问题。
fn cancel(cx: &mut App, state: &Entity<AppState>) {
    close(cx, state);
}

fn close(cx: &mut App, state: &Entity<AppState>) {
    // 先放开 swallow，再拆窗口：否则收尾期间用户按键会继续被吞。
    tap::set_panel_open(false);
    ui::window::close(cx, state);
    // 这里**不发 `cx.notify()`**：窗口在 `window::close` 里已经摘掉了，
    // 再通知重绘只会让 gpui 找不到窗口（日志里冒出 `window not found`）。
    // 清状态本身不需要重绘——`open = false` 之后调度器自然降回慢轮询。
    state.update(cx, |state, _| state.begin_close());
}

fn confirm(cx: &mut App, state: &Entity<AppState>, hit: crate::index::Hit) {
    // 提示必须在关窗之前算：`direct_hint` 要读 `config` 与 snippet 的触发词，
    // 关窗后状态就回到 Query 了（虽然字段还在，但先算完更不容易出错）。
    let (erase, config, fallback, hint, hint_ms) = {
        let state = state.read(cx);
        (
            state.erase,
            state.config.clone(),
            state.fallback_text(),
            state.direct_hint(&hit),
            state.config.hint_ms,
        )
    };

    close(cx, state);

    state.update(cx, |state, _| state.record_use(&hit.trigger));
    log::info!("展开 {}", hit.trigger);
    state::run_confirm(hit, erase, config, fallback);

    // 轻提示：告诉用户下次可以少开一次面板。时长可在偏好设置里改（默认 3 秒），
    // 设为 0 则完全不弹——有人就是不喜欢这种"事后提示"。
    if hint_ms > 0 {
        if let Some(text) = hint {
            ui::toast::open(cx, text);
            let tx = crate::app::event::tx(cx);
            std::thread::Builder::new()
                .name("semisnip-hint".into())
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(hint_ms));
                    if let Some(tx) = tx {
                        let _ = tx.send(Event::HintClose);
                    }
                })
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 闸门只吃 `(origin, erase)`，这里给一组最容易出错的值。
    fn defer_degraded(gate: &mut OpenGate, now: Instant) {
        gate.defer(Origin::EspansoShell, 0, now);
    }

    #[test]
    fn gate_holds_degraded_open_until_espanso_settles() {
        let mut gate = OpenGate::new();
        let t0 = Instant::now();
        defer_degraded(&mut gate, t0);

        // 落定窗口内不许开窗：这期间 espanso 还在删触发串。
        assert!(
            gate.take_due(t0 + Duration::from_millis(10)).is_none(),
            "早于 ESPANSO_SETTLE 时面板必须继续等"
        );
        // 到点放行一次。
        let due = t0 + ESPANSO_SETTLE;
        assert_eq!(gate.take_due(due), Some((Origin::EspansoShell, 0)));
        // 取走之后不能再放行第二次（否则面板会莫名重开）。
        assert!(gate.take_due(due + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn gate_keeps_the_first_pending_open() {
        let mut gate = OpenGate::new();
        let t0 = Instant::now();
        defer_degraded(&mut gate, t0);
        // 第二次唤醒不该把开窗时间往后推——否则连点两下会把面板一直拖着不开。
        defer_degraded(&mut gate, t0 + Duration::from_millis(100));

        assert!(
            gate.take_due(t0 + ESPANSO_SETTLE).is_some(),
            "按第一次的时间到期"
        );
    }

    #[test]
    fn clear_drops_pending_open() {
        let mut gate = OpenGate::new();
        let t0 = Instant::now();
        defer_degraded(&mut gate, t0);
        gate.clear();
        assert!(
            gate.take_due(t0 + Duration::from_secs(1)).is_none(),
            "取消 / 别的路径开窗之后，迟到的降级唤醒不许再把面板弹出来"
        );
    }

    /// 回归：曾经闸门放行的是 `Event`，调用方把它交回 `handle`，而 `handle`
    /// 见到 `EspansoShell` 又 `defer` 一次——每 150ms 自我推迟一次，面板永不
    /// 出现且没有任何日志。这条测试按"放行 → 如果还能被压回去会怎样"的形状写，
    /// 现在放行的已经是 `(origin, erase)`，压不回去。
    #[test]
    fn released_open_cannot_be_deferred_again() {
        let mut gate = OpenGate::new();
        let t0 = Instant::now();
        defer_degraded(&mut gate, t0);

        let released = gate.take_due(t0 + ESPANSO_SETTLE);
        assert_eq!(released, Some((Origin::EspansoShell, 0)));
        // 放行之后闸门必须是空的：如果哪天有人把 `take_due` 改回返回 `Event`
        // 并顺手 `handle`，这条断言会先红。
        assert!(gate.pending.is_none(), "放行即出闸，不许留残影");
    }

    #[test]
    fn retry_backoff_doubles_and_is_capped() {
        let mut delay = Duration::from_secs(1);
        for expected in [2, 4, 8, 10, 10] {
            delay = next_retry_delay(delay, false);
            assert_eq!(delay, Duration::from_secs(expected));
        }
        // 面板卡在授权页、系统已授权：立刻压回 1 秒，用户在等。
        assert_eq!(
            next_retry_delay(Duration::from_secs(10), true),
            Duration::from_secs(1)
        );
    }
}
