//! TurnRegistry —— 活动 turn 注册表 + AbortSignal（M3-B §1）
//!
//! # 职责
//!
//! 记录当前正在处理中的对话轮次（key = session_id），向命令层提供
//! 中止（abort）与 busy 口径（active_count）两个消费面：
//!
//! - `abort_turn` 命令：`abort(session_id)` 置位对应 turn 的 AbortSignal；
//! - 设置后端闸（set_settings）：`active_count() > 0` →
//!   拒绝修改（与前端 `Object.keys(processing).length > 0` 同口径）。
//!
//! 命令层 agent_process 在 turn 起点 `register`、process 返回后**无条件**
//! `unregister`（busy 释放点）。注册表只置标志，不碰 approvals/进程——
//! 清理收敛在 process 出口（单头清理，见 orchestrator 责任表⑦）。
//!
//! # 为何不用 CancellationToken
//!
//! 新依赖禁止（tokio_util 不引入）。用 `AtomicBool`（同步段轮询，
//! runner 100ms 轮询）+ `tokio::sync::Notify`（async 段 select 唤醒）
//! 复合表达同一信号：
//!
//! - 同步消费者（exec/runner 轮询循环）读 flag，零 await；
//! - 异步消费者（dispatch_turn / preflight 的 select）await `wait_abort()`，
//!   置位即唤醒。
//!
//! # 防漏唤醒（Notify 竞态）
//!
//! `Notify::notify_waiters` 只唤醒**已注册**的等待者，无等待者时通知即丢。
//! `wait_abort` 遵守「先查 flag → 注册 notified（poll 一次入队）→ 复查
//! flag → await」顺序：
//!
//! - abort 在注册前发生 → 复查兜底（flag 已置位，不 await）；
//! - abort 在注册后发生 → notify_waiters 必唤醒已入队的等待者。
//!
//! 任何交错下等待者都不会漏醒（见单测 wait_abort_* 系列）。

use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;
use tokio::sync::Notify;

// ═══════════════════════════════════════════════════════
// AbortSignal —— 单 turn 中止信号（flag + notify 复合）
// ═══════════════════════════════════════════════════════

/// 一次活动 turn 的中止信号（Clone：register 返回值与 ToolContext/runner
/// 注入共用同一底层状态）。
///
/// 语义：`abort()` 置位后**不可逆**（幂等，重复置位无害）；消费方各自
/// 按需读取——同步段轮询 `is_aborted()`，异步段 await `wait_abort()`。
#[derive(Debug, Clone)]
pub struct AbortSignal {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AbortSignal {
    /// 创建未置位的信号。
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// 同步查询是否已置位。
    pub fn is_aborted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 置位中止并唤醒全部等待者。
    ///
    /// 顺序固定：先写 flag 再 notify（消费方「复查 flag」依赖此顺序——
    /// 任何 notify 到达时 flag 必已可见）。
    pub fn abort(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 等待中止信号（async 段消费）。
    ///
    /// 保证：置位已发生时立即返回；未置位时挂起直到置位（不漏醒，见模块
    /// 头注释的防竞态顺序）。消费方按「返回后复查 is_aborted 定夺」使用。
    ///
    /// 一次 `wait_abort` 只消费一个置位事件；同一信号可被多个等待者
    /// 并发 await（notify_waiters 全量唤醒），也允许与同步轮询并存。
    pub async fn wait_abort(&self) {
        // ① 先查 flag：已置位立即返回（不再注册，避免无效等待）
        if self.is_aborted() {
            return;
        }
        // ② 注册：构造 notified 并 poll 一次进入 Notify 等待队列。
        //    仅创建不 poll 不算「已注册」——abort 恰在注册前
        //    notify_waiters 会丢（无等待者），必须以 poll 确立入队。
        let mut notified = Box::pin(self.notify.notified());
        poll_fn(|cx| {
            let _ = notified.as_mut().poll(cx);
            Poll::Ready(())
        })
        .await;
        // ③ 复查 flag：①~② 之间置位 → notify 已丢但 flag 兜底（权威）
        if self.is_aborted() {
            return;
        }
        // ④ await：此刻已入队；③ 之后的 abort 必 notify_waiters 唤醒本等待
        notified.await;
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════
// TurnRegistry —— 活动 turn 注册表
// ═══════════════════════════════════════════════════════

/// 单个活动 turn 的注册条目。
///
/// `conversation_id` / `started_at` 暂只记账不消费（多对话批量取消 /
/// 诊断为未来预留）；`signal` 是 abort 的落点（register 返回值同一对象）。
#[derive(Debug)]
pub struct ActiveTurn {
    #[allow(dead_code)] // 预留：多 turn 队列/诊断（M3-B 明确不做批量取消）
    pub conversation_id: String,
    #[allow(dead_code)] // 预留：诊断（age_of 类能力）
    pub started_at: Instant,
    /// 中止信号（register 返回值同一 Arc 底层）
    pub signal: AbortSignal,
    /// 后台 turn（记忆管线等）：可取消但**不计 busy**（active_count/is_active 不计）
    pub background: bool,
}

/// 活动 turn 注册表（key = session_id，单 Agent 悬浮窗下单会话单飞行）。
///
/// 内部 `std::sync::Mutex<HashMap>`：所有方法同步短持锁（无 await 跨越），
/// 与 orchestrator 的异步等待（Notify/oneshot）完全解耦。
#[derive(Debug, Default)]
pub struct TurnRegistry {
    turns: Mutex<HashMap<String, ActiveTurn>>,
}

impl TurnRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个活动 turn，返回中止信号句柄。
    ///
    /// 同一 session_id 重复注册（异常重入）：旧条目被替换并 **abort**——
    /// 旧 process 的取消点会因此收敛退出（防御；正常流程不可能重入——
    /// session_id 为前端每请求唯一标识，见 commands 层说明）。
    pub fn register(&self, session_id: &str, conversation_id: &str) -> AbortSignal {
        self.register_inner(session_id, conversation_id, false)
    }

    /// 注册一个**后台** turn（记忆管线等）：可经 `abort_turn` 取消，
    /// 但不计入 busy 口径（[`Self::active_count`] / [`Self::is_active`] 均不计）
    /// ——后台管线不拦任何用户操作（设计裁决 D3）。
    pub fn register_background(&self, session_id: &str, conversation_id: &str) -> AbortSignal {
        self.register_inner(session_id, conversation_id, true)
    }

    fn register_inner(
        &self,
        session_id: &str,
        conversation_id: &str,
        background: bool,
    ) -> AbortSignal {
        let signal = AbortSignal::new();
        let mut turns = self.turns.lock().unwrap();
        if let Some(old) = turns.insert(
            session_id.to_string(),
            ActiveTurn {
                conversation_id: conversation_id.to_string(),
                started_at: Instant::now(),
                signal: signal.clone(),
                background,
            },
        ) {
            old.signal.abort();
        }
        signal
    }

    /// 注销一个活动 turn（命令层 process 返回后无条件执行——busy 释放点）。
    ///
    /// 无此 session 时幂等（no-op）。
    pub fn unregister(&self, session_id: &str) {
        self.turns.lock().unwrap().remove(session_id);
    }

    /// 注销指定信号对应的 turn（防同名重入的旧 turn 误删新 turn）。
    ///
    /// 同一 session_id 重复注册时 `register` 会替换并 abort 旧条目；旧
    /// process 收尾时若仍按 session_id 注销，会把新条目一起删掉，导致
    /// busy 口径与 abort 通道提前失效。此入口只在当前条目仍是该信号时
    /// 删除，旧 process 的迟到注销因此成为 no-op。
    pub fn unregister_if(&self, session_id: &str, signal: &AbortSignal) {
        let mut turns = self.turns.lock().unwrap();
        let is_current = turns
            .get(session_id)
            .is_some_and(|turn| Arc::ptr_eq(&turn.signal.flag, &signal.flag));
        if is_current {
            turns.remove(session_id);
        }
    }

    /// 置位中止。返回 true = 找到活动 turn 并已置位；false = 无（幂等，
    /// abort_turn 命令以 Ok(false) 表达，非错误）。
    pub fn abort(&self, session_id: &str) -> bool {
        let found = self.turns.lock().unwrap().get(session_id).map(|t| t.signal.clone());
        match found {
            Some(signal) => {
                signal.abort();
                true
            }
            None => false,
        }
    }

    /// 活动 turn 数（后端 busy 口径：> 0 即「对话处理中」，供设置闸消费）。
    /// **不计后台 turn**（记忆管线可取消但不拦用户操作）。
    pub fn active_count(&self) -> usize {
        self.turns
            .lock()
            .unwrap()
            .values()
            .filter(|t| !t.background)
            .count()
    }

    /// 指定 session 是否有活动 turn。
    ///
    /// 消费端：steer_message 命令（仅活动 turn 可插话入队——空闲明确拒绝，
    /// 前端据此走关窗降级）；另在 cfg(test)（命令层守卫测试）。
    pub fn is_active(&self, session_id: &str) -> bool {
        // 后台管线不算「活动」（不可插话 / 不表示对话处理中）
        self.turns
            .lock()
            .unwrap()
            .get(session_id)
            .map(|t| !t.background)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── TurnRegistry ──

    #[test]
    fn register_unregister_roundtrip() {
        let reg = TurnRegistry::new();
        assert_eq!(reg.active_count(), 0);
        let signal = reg.register("s1", "conv-1");
        assert!(reg.is_active("s1"));
        assert_eq!(reg.active_count(), 1);
        assert!(!signal.is_aborted());
        // 未注册 session 隔离
        assert!(!reg.is_active("s2"));

        reg.unregister("s1");
        assert!(!reg.is_active("s1"));
        assert_eq!(reg.active_count(), 0);
        // 重复注销幂等
        reg.unregister("s1");
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn abort_found_sets_signal_and_unregistered_returns_false() {
        let reg = TurnRegistry::new();
        assert!(!reg.abort("nope"), "无活动 turn → false（幂等语义）");

        let signal = reg.register("s1", "conv-1");
        assert!(reg.abort("s1"), "找到活动 turn → true");
        assert!(signal.is_aborted(), "abort 应置位 register 返回的信号");
        // 重复 abort（turn 仍在注册表内）→ 幂等 true，无害
        assert!(reg.abort("s1"));

        reg.unregister("s1");
        assert!(!reg.abort("s1"), "注销后再 abort → false");
    }

    #[test]
    fn duplicate_register_replaces_and_aborts_old_entry() {
        let reg = TurnRegistry::new();
        let first = reg.register("s1", "conv-1");
        assert!(!first.is_aborted());
        let second = reg.register("s1", "conv-1");
        assert_eq!(reg.active_count(), 1, "同 session 重复注册应替换而非累积");
        assert!(first.is_aborted(), "旧条目应被 abort（防御：旧 process 收敛退出）");
        assert!(!second.is_aborted(), "新条目不受影响");
        // abort 命中新条目
        assert!(reg.abort("s1"));
        assert!(second.is_aborted());
    }

    #[test]
    fn stale_unregister_does_not_remove_new_turn() {
        let reg = TurnRegistry::new();
        let first = reg.register("s1", "conv-1");
        let second = reg.register("s1", "conv-1");
        assert_eq!(reg.active_count(), 1);

        // 旧 turn 迟到收尾：不能把新 turn 的 busy 位删掉
        reg.unregister_if("s1", &first);
        assert_eq!(reg.active_count(), 1, "旧信号不得注销新条目");
        assert!(reg.is_active("s1"));

        // 新 turn 仍可被 abort；正常收尾按自身信号注销
        assert!(reg.abort("s1"));
        assert!(second.is_aborted());
        reg.unregister_if("s1", &second);
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn active_count_tracks_multiple_sessions() {
        let reg = TurnRegistry::new();
        reg.register("s1", "c1");
        reg.register("s2", "c2");
        assert_eq!(reg.active_count(), 2);
        reg.unregister("s1");
        assert_eq!(reg.active_count(), 1);
        assert!(!reg.is_active("s1"));
        assert!(reg.is_active("s2"));
        reg.unregister("s2");
        assert_eq!(reg.active_count(), 0);
    }

    // ── AbortSignal.wait_abort（防漏唤醒竞态）──

    #[tokio::test]
    async fn wait_abort_after_abort_returns_immediately() {
        let signal = AbortSignal::new();
        signal.abort();
        // 置位后调用：立即返回
        tokio::time::timeout(Duration::from_secs(1), signal.wait_abort())
            .await
            .expect("置位后 wait_abort 应立即可用");
    }

    #[tokio::test]
    async fn wait_abort_before_abort_pends_then_wakes() {
        let signal = AbortSignal::new();
        let waiter = tokio::spawn({
            let s = signal.clone();
            async move {
                s.wait_abort().await;
            }
        });
        // 等待者已注册后置位 → 必被唤醒
        tokio::time::sleep(Duration::from_millis(50)).await;
        signal.abort();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("wait_abort 应被唤醒（不超时）")
            .expect("等待任务不应 panic");
    }

    #[tokio::test]
    async fn abort_before_wait_registration_returns_via_recheck() {
        // abort 发生在 wait_abort「先查」之后、「注册」之前（最窄竞态窗）：
        // notify_waiters 无等待者可丢，但注册后的复查必须兜底返回。
        for i in 0..100usize {
            let signal = AbortSignal::new();
            let waiter = tokio::spawn({
                let s = signal.clone();
                async move {
                    s.wait_abort().await;
                }
            });
            // 延迟错开：i 小 → abort 极可能早于 waiter 入队；i 大 → 晚于入队。
            // 两种落点都不得漏醒（本断言是活性测试，fail 方向不会抖动）。
            tokio::time::sleep(Duration::from_micros((i % 10) as u64 * 30)).await;
            signal.abort();
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap_or_else(|_| panic!("第 {i} 轮 wait_abort 漏唤醒"))
                .unwrap();
        }
    }

    #[tokio::test]
    async fn abort_wakes_all_waiters() {
        let signal = AbortSignal::new();
        let mut waiters = Vec::new();
        for _ in 0..8 {
            let s = signal.clone();
            waiters.push(tokio::spawn(async move { s.wait_abort().await }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        signal.abort();
        for w in waiters {
            tokio::time::timeout(Duration::from_secs(1), w)
                .await
                .expect("notify_waiters 应唤醒全部等待者")
                .unwrap();
        }
    }

    #[test]
    fn abort_is_idempotent_and_irreversible() {
        let signal = AbortSignal::new();
        signal.abort();
        signal.abort();
        assert!(signal.is_aborted());
        // 消费方（dispatch/runner）读到的都是同一底层状态（Clone 共享）
        let clone = signal.clone();
        assert!(clone.is_aborted());
    }
}
