//! Dual-mode tick-accurate task and timer service for host and plugins.
//!
//! Supports both discrete physics/frame ticks (`Ticks(u64)`) and continuous
//! floating-point durations (`Duration`), cancellation handles, recurring intervals,
//! and session-bound safety guards to prevent Slot Recycling Hazards.

use goldsrc_api::client::PlayerSessionToken;
use goldsrc_api::consts::log_targets;
pub use goldsrc_api::timer::{
    IntoScheduleDelay, ScheduleDelay, Ticks, TimerAction, TimerBound, TimerId, TimerMode,
};
use std::collections::{BinaryHeap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use stitch_rs::define_pipeline;
pub use stitch_rs::flow::FlowControl;

/// Trigger context intent representing an expiring timer or task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerTriggerIntent {
    pub id: TimerId,
    pub is_recurring: bool,
}

/// 64-byte aligned execution context for timer trigger operations conforming to SMA.
#[repr(C, align(64))]
pub struct TimerTriggerContext {
    pub id: TimerId,
    pub tick: u64,
    pub time: f64,
    pub is_suppressed: bool,
}

impl stitch_rs::Blackboard for TimerTriggerContext {}

impl TimerTriggerContext {
    #[inline(always)]
    pub fn new(id: TimerId, tick: u64, time: f64) -> Self {
        Self {
            id,
            tick,
            time,
            is_suppressed: false,
        }
    }

    #[inline(always)]
    pub fn suppress(&mut self) {
        self.is_suppressed = true;
    }
}

define_pipeline! {
    name: Timer,
    context: TimerTriggerContext,
    intent: TimerTriggerIntent,
    outcome: (),
    error: (),
    vis: pub,
}

static TIMER_PIPELINE: LazyLock<RwLock<TimerPipeline>> =
    LazyLock::new(|| RwLock::new(TimerPipeline::new()));

/// Registers a custom middleware layer with the global timer pipeline.
pub fn register_timer_layer(layer: Arc<dyn TimerLayer>) {
    let mut lock = TIMER_PIPELINE.write().unwrap_or_else(|e| e.into_inner());
    *lock = lock.clone().with_layer(layer);
}

/// Dispatches a timer trigger through the global SMA U-cycle pipeline.
#[allow(clippy::result_unit_err)]
pub fn dispatch_timer_trigger(
    ctx: &mut TimerTriggerContext,
    intent: &TimerTriggerIntent,
) -> Result<(), ()> {
    let pipeline = TIMER_PIPELINE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    pipeline.dispatch(ctx, intent)
}

type TaskCallback = Box<dyn FnMut() -> TimerAction + Send + 'static>;

struct TickEntry {
    id: TimerId,
    target_tick: u64,
    mode: TimerMode,
    bound: TimerBound,
    callback: TaskCallback,
}

impl PartialEq for TickEntry {
    fn eq(&self, other: &Self) -> bool {
        self.target_tick == other.target_tick && self.id == other.id
    }
}

impl Eq for TickEntry {}

impl PartialOrd for TickEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TickEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering so BinaryHeap acts as a min-heap
        other
            .target_tick
            .cmp(&self.target_tick)
            .then_with(|| other.id.cmp(&self.id))
    }
}

struct TimeEntry {
    id: TimerId,
    target_time: f64,
    mode: TimerMode,
    bound: TimerBound,
    callback: TaskCallback,
}

impl PartialEq for TimeEntry {
    fn eq(&self, other: &Self) -> bool {
        self.target_time.to_bits() == other.target_time.to_bits() && self.id == other.id
    }
}

impl Eq for TimeEntry {}

impl PartialOrd for TimeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimeEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering so BinaryHeap acts as a min-heap
        other
            .target_time
            .partial_cmp(&self.target_time)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| other.id.cmp(&self.id))
    }
}

struct InnerQueues {
    tick_heap: BinaryHeap<TickEntry>,
    time_heap: BinaryHeap<TimeEntry>,
    cancelled: HashSet<TimerId>,
}

/// Thread-safe, frame-accurate timer and task scheduler.
pub struct TimerService {
    next_id: AtomicU64,
    current_tick: AtomicU64,
    current_time: Mutex<f64>,
    queues: Mutex<InnerQueues>,
}

impl Default for TimerService {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerService {
    /// Creates a new empty `TimerService`.
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            current_tick: AtomicU64::new(0),
            current_time: Mutex::new(0.0),
            queues: Mutex::new(InnerQueues {
                tick_heap: BinaryHeap::new(),
                time_heap: BinaryHeap::new(),
                cancelled: HashSet::new(),
            }),
        }
    }

    /// Returns the current discrete tick count.
    #[inline]
    pub fn current_tick(&self) -> u64 {
        self.current_tick.load(Ordering::Relaxed)
    }

    /// Returns the current continuous time in seconds.
    #[inline]
    pub fn current_time(&self) -> f64 {
        *self.current_time.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Allocates a new unique `TimerId`.
    fn next_timer_id(&self) -> TimerId {
        TimerId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Schedules a task with custom delay, repetition mode, and session binding.
    pub fn schedule<D, F>(
        &self,
        delay: D,
        mode: TimerMode,
        bound: TimerBound,
        callback: F,
    ) -> TimerId
    where
        D: IntoScheduleDelay,
        F: FnMut() -> TimerAction + Send + 'static,
    {
        let id = self.next_timer_id();
        let delay = delay.into_schedule_delay();
        let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());

        match delay {
            ScheduleDelay::Ticks(ticks) => {
                let target_tick = self.current_tick().saturating_add(ticks);
                queues.tick_heap.push(TickEntry {
                    id,
                    target_tick,
                    mode,
                    bound,
                    callback: Box::new(callback),
                });
            }
            ScheduleDelay::Duration(dur) => {
                let target_time = self.current_time() + dur.as_secs_f64();
                queues.time_heap.push(TimeEntry {
                    id,
                    target_time,
                    mode,
                    bound,
                    callback: Box::new(callback),
                });
            }
        }

        id
    }

    /// Schedules a one-shot task to execute after the specified delay.
    pub fn after<D, F>(&self, delay: D, callback: F) -> TimerId
    where
        D: IntoScheduleDelay,
        F: FnOnce() + Send + 'static,
    {
        let mut cb = Some(callback);
        self.schedule(delay, TimerMode::Once, TimerBound::Global, move || {
            if let Some(f) = cb.take() {
                f();
            }
            TimerAction::Stop
        })
    }

    /// Schedules a recurring task repeating at the specified interval.
    pub fn every<D, F>(&self, interval: D, callback: F) -> TimerId
    where
        D: IntoScheduleDelay,
        F: FnMut() -> TimerAction + Send + 'static,
    {
        let delay = interval.into_schedule_delay();
        self.schedule(
            delay,
            TimerMode::Recurring(delay),
            TimerBound::Global,
            callback,
        )
    }

    /// Schedules a one-shot task bound to a specific player session token.
    ///
    /// If the player disconnects or reconnects (generating a new session token),
    /// the task will be automatically discarded without running.
    pub fn after_bound<D, F>(&self, delay: D, token: PlayerSessionToken, callback: F) -> TimerId
    where
        D: IntoScheduleDelay,
        F: FnOnce() + Send + 'static,
    {
        let mut cb = Some(callback);
        self.schedule(
            delay,
            TimerMode::Once,
            TimerBound::Session(token),
            move || {
                if let Some(f) = cb.take() {
                    f();
                }
                TimerAction::Stop
            },
        )
    }

    /// Schedules a recurring task repeating at the specified interval bound to a player session.
    pub fn every_bound<D, F>(&self, interval: D, token: PlayerSessionToken, callback: F) -> TimerId
    where
        D: IntoScheduleDelay,
        F: FnMut() -> TimerAction + Send + 'static,
    {
        let delay = interval.into_schedule_delay();
        self.schedule(
            delay,
            TimerMode::Recurring(delay),
            TimerBound::Session(token),
            callback,
        )
    }

    /// Cancels a scheduled timer by its ID. Returns `true` if registered.
    pub fn cancel(&self, id: TimerId) -> bool {
        let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        queues.cancelled.insert(id)
    }

    /// Returns `true` if the specified timer is marked as cancelled.
    pub fn is_cancelled(&self, id: TimerId) -> bool {
        let queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        queues.cancelled.contains(&id)
    }

    /// Advances the timer clock and executes all expired tasks.
    ///
    /// # Safety and Re-entrancy
    /// To ensure complete re-entrancy and prevent deadlocks, the internal queue
    /// lock is dropped during callback execution. Callbacks are wrapped in
    /// `std::panic::catch_unwind` to prevent panic propagation across the engine boundary.
    pub fn tick<V>(&self, current_tick: u64, current_time: f64, session_validator: V) -> usize
    where
        V: Fn(PlayerSessionToken) -> bool,
    {
        self.current_tick.store(current_tick, Ordering::Relaxed);
        {
            let mut time_guard = self.current_time.lock().unwrap_or_else(|e| e.into_inner());
            *time_guard = current_time;
        }

        // 0. Zero-allocation fast path for idle frames
        let has_work = {
            let queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            let expired_ticks = queues
                .tick_heap
                .peek()
                .is_some_and(|top| top.target_tick <= current_tick);
            let expired_times = queues
                .time_heap
                .peek()
                .is_some_and(|top| top.target_time <= current_time);
            expired_ticks || expired_times
        };

        if !has_work {
            return 0;
        }

        // 1. Drain expired tick and time items under lock
        let (expired_ticks, expired_times) = {
            let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            let mut ticks = Vec::new();
            while let Some(top) = queues.tick_heap.peek() {
                if top.target_tick <= current_tick {
                    let item = queues.tick_heap.pop().unwrap();
                    if queues.cancelled.remove(&item.id) {
                        continue;
                    }
                    ticks.push(item);
                } else {
                    break;
                }
            }

            let mut times = Vec::new();
            while let Some(top) = queues.time_heap.peek() {
                if top.target_time <= current_time {
                    let item = queues.time_heap.pop().unwrap();
                    if queues.cancelled.remove(&item.id) {
                        continue;
                    }
                    times.push(item);
                } else {
                    break;
                }
            }

            (ticks, times)
        };

        let mut executed_count = 0;
        let mut reschedule_ticks = Vec::new();
        let mut reschedule_times = Vec::new();

        // 2. Execute tick callbacks outside lock
        for mut entry in expired_ticks {
            if let TimerBound::Session(token) = entry.bound
                && !session_validator(token)
            {
                log::debug!(
                    target: log_targets::CORE,
                    "Timer {:?} skipped due to expired session token {:?}",
                    entry.id,
                    token
                );
                continue;
            }

            let is_recurring = matches!(entry.mode, TimerMode::Recurring(_));
            let intent = TimerTriggerIntent {
                id: entry.id,
                is_recurring,
            };
            let mut ctx = TimerTriggerContext::new(entry.id, current_tick, current_time);

            if dispatch_timer_trigger(&mut ctx, &intent).is_err() {
                log::debug!(
                    target: log_targets::CORE,
                    "Timer {:?} suppressed by SMA timer pipeline",
                    entry.id
                );
                continue;
            }

            executed_count += 1;
            let action =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (entry.callback)()))
                {
                    Ok(act) => act,
                    Err(err) => {
                        log::error!(
                            target: log_targets::CORE,
                            "Timer {:?} panicked during execution: {:?}",
                            entry.id,
                            err
                        );
                        TimerAction::Stop
                    }
                };

            if action == TimerAction::Continue
                && let TimerMode::Recurring(interval) = entry.mode
            {
                match interval {
                    ScheduleDelay::Ticks(t) => {
                        entry.target_tick = current_tick.saturating_add(t);
                        reschedule_ticks.push(entry);
                    }
                    ScheduleDelay::Duration(d) => {
                        reschedule_times.push(TimeEntry {
                            id: entry.id,
                            target_time: current_time + d.as_secs_f64(),
                            mode: entry.mode,
                            bound: entry.bound,
                            callback: entry.callback,
                        });
                    }
                }
            }
        }

        // 3. Execute time callbacks outside lock
        for mut entry in expired_times {
            if let TimerBound::Session(token) = entry.bound
                && !session_validator(token)
            {
                log::debug!(
                    target: log_targets::CORE,
                    "Timer {:?} skipped due to expired session token {:?}",
                    entry.id,
                    token
                );
                continue;
            }

            let is_recurring = matches!(entry.mode, TimerMode::Recurring(_));
            let intent = TimerTriggerIntent {
                id: entry.id,
                is_recurring,
            };
            let mut ctx = TimerTriggerContext::new(entry.id, current_tick, current_time);

            if dispatch_timer_trigger(&mut ctx, &intent).is_err() {
                log::debug!(
                    target: log_targets::CORE,
                    "Timer {:?} suppressed by SMA timer pipeline",
                    entry.id
                );
                continue;
            }

            executed_count += 1;
            let action =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (entry.callback)()))
                {
                    Ok(act) => act,
                    Err(err) => {
                        log::error!(
                            target: log_targets::CORE,
                            "Timer {:?} panicked during execution: {:?}",
                            entry.id,
                            err
                        );
                        TimerAction::Stop
                    }
                };

            if action == TimerAction::Continue
                && let TimerMode::Recurring(interval) = entry.mode
            {
                match interval {
                    ScheduleDelay::Ticks(t) => {
                        reschedule_ticks.push(TickEntry {
                            id: entry.id,
                            target_tick: current_tick.saturating_add(t),
                            mode: entry.mode,
                            bound: entry.bound,
                            callback: entry.callback,
                        });
                    }
                    ScheduleDelay::Duration(d) => {
                        entry.target_time = current_time + d.as_secs_f64();
                        reschedule_times.push(entry);
                    }
                }
            }
        }

        // 4. Re-enqueue repeating tasks under lock
        if !reschedule_ticks.is_empty() || !reschedule_times.is_empty() {
            let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            for item in reschedule_ticks {
                if !queues.cancelled.remove(&item.id) {
                    queues.tick_heap.push(item);
                }
            }
            for item in reschedule_times {
                if !queues.cancelled.remove(&item.id) {
                    queues.time_heap.push(item);
                }
            }
        }

        executed_count
    }
}

static GLOBAL_TIMER_SERVICE: OnceLock<Arc<TimerService>> = OnceLock::new();

/// Returns the global host `TimerService` instance.
pub fn timer_service() -> Arc<TimerService> {
    GLOBAL_TIMER_SERVICE
        .get_or_init(|| Arc::new(TimerService::new()))
        .clone()
}

/// Convenience function: schedules a one-shot task on the global host timer service.
pub fn after<D, F>(delay: D, callback: F) -> TimerId
where
    D: IntoScheduleDelay,
    F: FnOnce() + Send + 'static,
{
    timer_service().after(delay, callback)
}

/// Convenience function: schedules a recurring task on the global host timer service.
pub fn every<D, F>(interval: D, callback: F) -> TimerId
where
    D: IntoScheduleDelay,
    F: FnMut() -> TimerAction + Send + 'static,
{
    timer_service().every(interval, callback)
}

/// Convenience function: cancels a timer on the global host timer service.
pub fn cancel(id: TimerId) -> bool {
    timer_service().cancel(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Duration;

    #[test]
    fn test_tick_scheduling_and_execution() {
        let service = TimerService::new();
        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = fired.clone();

        service.after(Ticks(5), move || {
            f_clone.store(true, Ordering::SeqCst);
        });

        assert_eq!(service.tick(1, 0.1, |_| true), 0);
        assert!(!fired.load(Ordering::SeqCst));

        assert_eq!(service.tick(4, 0.4, |_| true), 0);
        assert!(!fired.load(Ordering::SeqCst));

        assert_eq!(service.tick(5, 0.5, |_| true), 1);
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_duration_scheduling_and_execution() {
        let service = TimerService::new();
        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = fired.clone();

        service.after(Duration::from_millis(500), move || {
            f_clone.store(true, Ordering::SeqCst);
        });

        assert_eq!(service.tick(1, 0.2, |_| true), 0);
        assert!(!fired.load(Ordering::SeqCst));

        assert_eq!(service.tick(2, 0.51, |_| true), 1);
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_recurring_timer_with_stop() {
        let service = TimerService::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c_clone = counter.clone();

        service.every(Ticks(2), move || {
            let count = c_clone.fetch_add(1, Ordering::SeqCst) + 1;
            if count >= 3 {
                TimerAction::Stop
            } else {
                TimerAction::Continue
            }
        });

        assert_eq!(service.tick(1, 0.0, |_| true), 0);
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        assert_eq!(service.tick(2, 0.0, |_| true), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        assert_eq!(service.tick(3, 0.0, |_| true), 0);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        assert_eq!(service.tick(4, 0.0, |_| true), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 2);

        assert_eq!(service.tick(6, 0.0, |_| true), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 3);

        // Stopped now, won't fire at tick 8
        assert_eq!(service.tick(8, 0.0, |_| true), 0);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_session_bound_invalidation_slot_recycling() {
        let service = TimerService::new();
        let token_alice = PlayerSessionToken::new(1, 1, 100);
        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = fired.clone();

        service.after_bound(Ticks(3), token_alice, move || {
            f_clone.store(true, Ordering::SeqCst);
        });

        // Simulate Alice disconnected and Bob connected (generation changed from 1 to 2)
        let active_token_bob = PlayerSessionToken::new(1, 2, 200);

        let count = service.tick(3, 0.0, |token| token == active_token_bob);
        assert_eq!(count, 0); // Alice's timer was skipped
        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_cancellation() {
        let service = TimerService::new();
        let fired = Arc::new(AtomicBool::new(false));
        let f_clone = fired.clone();

        let id = service.after(Ticks(1), move || {
            f_clone.store(true, Ordering::SeqCst);
        });

        assert!(service.cancel(id));
        assert_eq!(service.tick(1, 0.0, |_| true), 0);
        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_reentrancy_scheduling_from_callback() {
        let service = Arc::new(TimerService::new());
        let s_clone = service.clone();
        let second_fired = Arc::new(AtomicBool::new(false));
        let sf_clone = second_fired.clone();

        service.after(Ticks(1), move || {
            s_clone.after(Ticks(2), move || {
                sf_clone.store(true, Ordering::SeqCst);
            });
        });

        assert_eq!(service.tick(1, 0.0, |_| true), 1);
        assert!(!second_fired.load(Ordering::SeqCst));

        assert_eq!(service.tick(3, 0.0, |_| true), 1);
        assert!(second_fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_timer_context_alignment() {
        assert_eq!(
            std::mem::align_of::<TimerTriggerContext>(),
            64,
            "TimerTriggerContext must have 64-byte cache line alignment per Scrooge invariant"
        );
    }

    struct MockAuditLayer {
        entered: Arc<std::sync::atomic::AtomicUsize>,
        exited: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl TimerLayer for MockAuditLayer {
        fn on_enter(
            &self,
            _ctx: &mut TimerTriggerContext,
            _intent: &TimerTriggerIntent,
        ) -> FlowControl<(), (), ()> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            FlowControl::Proceed(())
        }

        fn on_exit(
            &self,
            _ctx: &mut TimerTriggerContext,
            _intent: &TimerTriggerIntent,
            outcome: &mut Result<(), ()>,
        ) {
            self.exited.fetch_add(1, Ordering::SeqCst);
            assert!(outcome.is_ok());
        }
    }

    #[test]
    fn test_timer_pipeline_u_cycle() {
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let exited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let layer = Arc::new(MockAuditLayer {
            entered: entered.clone(),
            exited: exited.clone(),
        });

        let pipeline = TimerPipeline::new().with_layer(layer);
        let mut ctx = TimerTriggerContext::new(TimerId(1), 10, 0.5);
        let intent = TimerTriggerIntent {
            id: TimerId(1),
            is_recurring: false,
        };

        let res = pipeline.dispatch(&mut ctx, &intent);
        assert!(res.is_ok());
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        assert_eq!(exited.load(Ordering::SeqCst), 1);
    }
}
