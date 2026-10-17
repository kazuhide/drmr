//! Bounded parallel work with ordered results and deterministic failure selection.
//!
//! Workers take queued tasks in submission order. After a failure, no queued work starts;
//! already-running work drains. The earliest failing submitted task determines the
//! returned error. Panics set the stop flag and resume on the caller's thread when selected.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::Scope;
use std::time::{Duration, Instant};

/// The cap of the default pool size (`concurrency.default_workers`); an explicit value is
/// bounded separately, at 64.
pub const DEFAULT_WORKERS_CAP: usize = 32;
/// Maximum submitted but unconsumed units per ordered coordinator. Producers stay lazy.
pub const MAX_PENDING_TASKS: usize = 128;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking unit never holds these locks (units run outside them), so a poisoned lock
    // only means another thread panicked elsewhere; the state it guards is still consistent.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A cooperative stop flag units may poll or wait on; set as soon as one unit fails
/// (`concurrency.Stop`, a `threading.Event`). Clones share the flag.
#[derive(Clone, Default)]
pub struct Stop {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl fmt::Debug for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stop").field("set", &self.is_set()).finish()
    }
}

impl Stop {
    /// A flag that is not set.
    pub fn new() -> Self {
        Stop::default()
    }

    /// Set the flag and wake every waiter.
    pub fn set(&self) {
        let (flag, changed) = &*self.inner;
        *lock(flag) = true;
        changed.notify_all();
    }

    /// Whether the flag is set.
    pub fn is_set(&self) -> bool {
        *lock(&self.inner.0)
    }

    /// Block until the flag is set or `timeout` passes (`None`: no limit); returns whether it
    /// is set.
    pub fn wait(&self, timeout: Option<Duration>) -> bool {
        let (flag, changed) = &*self.inner;
        let mut set = lock(flag);
        let deadline = timeout.map(|limit| Instant::now() + limit);
        while !*set {
            match deadline {
                None => set = changed.wait(set).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    set = changed
                        .wait_timeout(set, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
        true
    }
}

/// Returned by a unit that stopped because another unit failed; never the reimplemented failure
/// (`concurrency.Cancelled`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// The error side of a unit run by [`run_ordered`]: a failure, or [`Cancelled`].
pub trait UnitError: Sized {
    /// The error a unit returns when it observed the stop flag (Python's `Cancelled`).
    fn cancelled() -> Self;
    /// Whether this error is [`Cancelled`].
    fn is_cancelled(&self) -> bool;
}

impl UnitError for Cancelled {
    fn cancelled() -> Self {
        Cancelled
    }

    fn is_cancelled(&self) -> bool {
        true
    }
}

/// The default size of a CPU-bound pool: the CPUs this process may run on (the affinity mask
/// where the platform has one, else the CPU count), at most 32. A CFS quota (`docker --cpus`)
/// is not visible here; the CLI's `--max-workers` and the console's
/// `DRMR_MAX_WORKERS` bound the pools explicitly. (A value given explicitly is
/// bounded separately, at 64.)
pub fn default_workers() -> usize {
    workers_for(affinity_cpus(), cpu_count)
}

/// `default_workers` over its inputs: `affinity` is the size of the affinity mask when the
/// platform has one (`len(os.sched_getaffinity(0))`), `cpu_count` is `os.cpu_count` (asked
/// only without a mask). `max(1, min(32, cpus or 4))`, where a missing or zero count is 4.
pub fn workers_for(affinity: Option<usize>, cpu_count: impl FnOnce() -> Option<usize>) -> usize {
    let cpus = match affinity {
        Some(count) => count,
        None => cpu_count().unwrap_or(0),
    };
    let cpus = if cpus == 0 { 4 } else { cpus };
    cpus.clamp(1, DEFAULT_WORKERS_CAP)
}

/// `len(os.sched_getaffinity(0))` where the platform has it (Linux), else `None` (Python's
/// `AttributeError`); a failing call is `None` too (its `OSError`).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn affinity_cpus() -> Option<usize> {
    rustix::thread::sched_getaffinity(None)
        .ok()
        .map(|set| usize::try_from(set.count()).unwrap_or(usize::MAX))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn affinity_cpus() -> Option<usize> {
    None
}

/// `os.cpu_count`: the online CPUs (`sysconf(_SC_NPROCESSORS_ONLN)`, which is what Rust's
/// `available_parallelism` reads on every platform without an affinity mask; on Linux it is
/// asked only when `sched_getaffinity` failed).
fn cpu_count() -> Option<usize> {
    std::thread::available_parallelism()
        .ok()
        .map(std::num::NonZero::get)
}

/// How a unit ended: its value, or the payload of its panic.
pub enum Outcome<T> {
    /// The unit returned.
    Returned(T),
    /// The unit panicked (Python's `BaseException` escaping a unit).
    Panicked(Box<dyn Any + Send>),
}

impl<T> fmt::Debug for Outcome<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Returned(_) => f.write_str("Returned(..)"),
            Outcome::Panicked(_) => f.write_str("Panicked(..)"),
        }
    }
}

impl<T> Outcome<T> {
    /// The value, resuming the panic on this thread when the unit panicked.
    pub fn into_value(self) -> T {
        match self {
            Outcome::Returned(value) => value,
            Outcome::Panicked(payload) => panic::resume_unwind(payload),
        }
    }
}

enum TaskState<T> {
    Pending,
    Running,
    Finished(Outcome<T>),
    Taken,
    Cancelled,
}

type Callback = Box<dyn FnOnce() + Send>;

struct TaskInner<T> {
    state: Mutex<(TaskState<T>, Vec<Callback>)>,
    done: Condvar,
}

/// A unit submitted to a [`Pool`] (`concurrent.futures.Future`). Clones share the unit.
pub struct Task<T> {
    inner: Arc<TaskInner<T>>,
}

impl<T> Clone for Task<T> {
    fn clone(&self) -> Self {
        Task {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match lock(&self.inner.state).0 {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Finished(_) | TaskState::Taken => "finished",
            TaskState::Cancelled => "cancelled",
        };
        f.debug_struct("Task").field("state", &state).finish()
    }
}

impl<T> Task<T> {
    fn new() -> Self {
        Task {
            inner: Arc::new(TaskInner {
                state: Mutex::new((TaskState::Pending, Vec::new())),
                done: Condvar::new(),
            }),
        }
    }

    /// Run `unit` unless the task was cancelled first (`set_running_or_notify_cancel`).
    fn run(&self, unit: impl FnOnce() -> T) {
        {
            let mut state = lock(&self.inner.state);
            match state.0 {
                TaskState::Pending => state.0 = TaskState::Running,
                _ => return,
            }
        }
        let outcome = match panic::catch_unwind(AssertUnwindSafe(unit)) {
            Ok(value) => Outcome::Returned(value),
            Err(payload) => Outcome::Panicked(payload),
        };
        self.finish(TaskState::Finished(outcome));
    }

    fn finish(&self, end: TaskState<T>) {
        let callbacks = {
            let mut state = lock(&self.inner.state);
            state.0 = end;
            std::mem::take(&mut state.1)
        };
        self.inner.done.notify_all();
        for callback in callbacks {
            callback();
        }
    }

    /// Cancel the unit if it has not started (`Future.cancel`): `true` when it is cancelled
    /// (now or before), `false` when it is running or finished.
    pub fn cancel(&self) -> bool {
        {
            let state = lock(&self.inner.state);
            match state.0 {
                TaskState::Cancelled => return true,
                TaskState::Pending => {}
                _ => return false,
            }
        }
        self.finish(TaskState::Cancelled);
        true
    }

    /// Whether the unit was cancelled before it started.
    pub fn cancelled(&self) -> bool {
        matches!(lock(&self.inner.state).0, TaskState::Cancelled)
    }

    /// Whether the unit finished or was cancelled.
    pub fn done(&self) -> bool {
        matches!(
            lock(&self.inner.state).0,
            TaskState::Finished(_) | TaskState::Taken | TaskState::Cancelled
        )
    }

    /// Block until the unit finished or was cancelled.
    pub fn wait(&self) {
        let mut state = lock(&self.inner.state);
        while matches!(state.0, TaskState::Pending | TaskState::Running) {
            state = self
                .inner
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Call `callback` once the unit is done (`add_done_callback`): at once when it already is.
    pub fn on_done(&self, callback: impl FnOnce() + Send + 'static) {
        {
            let mut state = lock(&self.inner.state);
            if matches!(state.0, TaskState::Pending | TaskState::Running) {
                state.1.push(Box::new(callback));
                return;
            }
        }
        callback();
    }

    /// Look at how the unit ended without taking the result: `None` while it is not done, when
    /// it was cancelled, or once the result was taken.
    pub fn peek<R>(&self, look: impl FnOnce(Option<&Outcome<T>>) -> R) -> R {
        let state = lock(&self.inner.state);
        match &state.0 {
            TaskState::Finished(outcome) => look(Some(outcome)),
            _ => look(None),
        }
    }

    /// Take how the unit ended (once): `None` while it is not done, when it was cancelled, or
    /// when the result was taken already.
    pub fn take(&self) -> Option<Outcome<T>> {
        let mut state = lock(&self.inner.state);
        match std::mem::replace(&mut state.0, TaskState::Taken) {
            TaskState::Finished(outcome) => Some(outcome),
            other => {
                state.0 = other;
                None
            }
        }
    }
}

trait QueuedUnit: Send {
    fn run(self: Box<Self>);
    fn cancel(self: Box<Self>);
}

struct Queued<T, F> {
    task: Task<T>,
    unit: F,
}

impl<T: Send, F: FnOnce() -> T + Send> QueuedUnit for Queued<T, F> {
    fn run(self: Box<Self>) {
        let Queued { task, unit } = *self;
        task.run(unit);
    }

    fn cancel(self: Box<Self>) {
        self.task.cancel();
    }
}

struct Queue<'scope> {
    units: VecDeque<Box<dyn QueuedUnit + 'scope>>,
    closed: bool,
}

/// A bounded pool of worker threads inside a [`std::thread::scope`], taking units in
/// submission order (`ThreadPoolExecutor(max_workers=…)`). Units may borrow from the scope's
/// environment. Dropping the pool shuts it down without cancelling (`shutdown(wait=True)`):
/// the queued units still run, and the scope joins the workers.
pub struct Pool<'scope> {
    shared: Arc<(Mutex<Queue<'scope>>, Condvar)>,
}

impl fmt::Debug for Pool<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let queue = lock(&self.shared.0);
        f.debug_struct("Pool")
            .field("queued", &queue.units.len())
            .field("closed", &queue.closed)
            .finish()
    }
}

impl<'scope> Pool<'scope> {
    /// A pool of `workers` threads (at least one) spawned in `scope`.
    pub fn new<'env>(scope: &'scope Scope<'scope, 'env>, workers: usize) -> Self {
        let shared = Arc::new((
            Mutex::new(Queue {
                units: VecDeque::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        for _ in 0..workers.max(1) {
            let shared = Arc::clone(&shared);
            scope.spawn(move || work(&shared));
        }
        Pool { shared }
    }

    /// Queue `unit`; a worker runs it once every unit submitted before it has started.
    /// Submitting to a pool that was shut down returns a cancelled task.
    pub fn submit<T: Send + 'scope>(&self, unit: impl FnOnce() -> T + Send + 'scope) -> Task<T> {
        let task = Task::new();
        {
            let mut queue = lock(&self.shared.0);
            if !queue.closed {
                queue.units.push_back(Box::new(Queued {
                    task: task.clone(),
                    unit,
                }));
                drop(queue);
                self.shared.1.notify_one();
                return task;
            }
        }
        task.cancel();
        task
    }

    /// Take no more units; with `cancel_queued`, every unit not yet started is cancelled
    /// (`shutdown(cancel_futures=True)`). The workers exit once the queue is empty; the scope
    /// joins them.
    pub fn shutdown(&self, cancel_queued: bool) {
        let cancelled: Vec<Box<dyn QueuedUnit + 'scope>> = {
            let mut queue = lock(&self.shared.0);
            queue.closed = true;
            if cancel_queued {
                queue.units.drain(..).collect()
            } else {
                Vec::new()
            }
        };
        self.shared.1.notify_all();
        for unit in cancelled {
            unit.cancel();
        }
    }
}

impl Drop for Pool<'_> {
    fn drop(&mut self) {
        self.shutdown(false);
    }
}

fn work(shared: &(Mutex<Queue<'_>>, Condvar)) {
    let (queue, available) = shared;
    loop {
        let unit = {
            let mut guard = lock(queue);
            loop {
                if let Some(unit) = guard.units.pop_front() {
                    break unit;
                }
                if guard.closed {
                    return;
                }
                guard = available
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        unit.run();
    }
}

/// A unit of [`run_ordered`].
pub type Unit<'a, T, E> = Box<dyn FnOnce() -> Result<T, E> + Send + 'a>;

/// Whether a finished task raised: it returned an error (a [`Cancelled`] one included) or
/// panicked (`_raised`: done, not cancelled, an exception).
fn raised<T, E>(task: &Task<Result<T, E>>) -> bool {
    task.peek(|outcome| {
        matches!(
            outcome,
            Some(Outcome::Returned(Err(_)) | Outcome::Panicked(_))
        )
    })
}

fn raised_for_real<T, E: UnitError>(task: &Task<Result<T, E>>) -> bool {
    task.peek(|outcome| match outcome {
        Some(Outcome::Returned(Err(error))) => !error.is_cancelled(),
        Some(Outcome::Panicked(_)) => true,
        _ => false,
    })
}

/// Run `units` through `submit` (a pool's `submit`) and return their results in order.
///
/// Fail-fast: the failing unit sets the stop flag (the given one, else a private one) as it
/// fails, every unit not yet started is cancelled, a unit the pool had already picked finds
/// the flag set as it starts and does nothing ('s start gate), the ones in flight drain,
/// and the earliest failure in submission order among the units that ran is returned (a panic
/// is resumed). A unit that failed only because it observed the stop flag returns
/// [`UnitError::cancelled`], which is never selected while a real failure exists.
pub fn run_ordered<'a, T, E, S>(
    mut submit: S,
    units: impl IntoIterator<Item = Unit<'a, T, E>>,
    stop: Option<&Stop>,
) -> Result<Vec<T>, E>
where
    T: Send + 'a,
    E: UnitError + Send + 'a,
    S: FnMut(Unit<'a, T, E>) -> Task<Result<T, E>>,
{
    let gate = stop.cloned().unwrap_or_default();
    let mut units = units.into_iter();
    let mut tasks: Vec<Task<Result<T, E>>> = Vec::new();
    let mut results = Vec::new();
    loop {
        while tasks.len() < MAX_PENDING_TASKS && (!gate.is_set() || tasks.is_empty()) {
            let Some(unit) = units.next() else {
                break;
            };
            let gate = gate.clone();
            let gated: Unit<'a, T, E> = Box::new(move || run_gated(&gate, unit));
            tasks.push(submit(gated));
        }
        if tasks.is_empty() {
            return Ok(results);
        }
        wait_window(&tasks);
        // Finished states are immutable. Snapshot the prefix before checking failures so
        // a concurrent failure outside that prefix cannot be consumed as a success.
        let ready = tasks.iter().take_while(|task| task.done()).count();
        if tasks.iter().any(raised) {
            gate.set();
            for task in &tasks {
                task.cancel();
            }
            for task in &tasks {
                task.wait();
            }
            let chosen = tasks
                .iter()
                .find(|task| raised_for_real(task))
                .or_else(|| tasks.iter().find(|task| raised(task)))
                .expect("a task raised");
            return match chosen.take() {
                Some(Outcome::Returned(Err(error))) => Err(error),
                Some(Outcome::Panicked(payload)) => panic::resume_unwind(payload),
                _ => unreachable!("the chosen task raised"),
            };
        }
        for task in tasks.drain(..ready) {
            match task.take().map(Outcome::into_value) {
                Some(Ok(value)) => results.push(value),
                _ => unreachable!("the completed prefix returned values"),
            }
        }
    }
}

/// The start gate around one unit: nothing runs once the flag is set; a failure (not a
/// [`Cancelled`] one) or a panic sets it.
fn run_gated<T, E: UnitError>(gate: &Stop, unit: Unit<'_, T, E>) -> Result<T, E> {
    if gate.is_set() {
        return Err(E::cancelled());
    }
    match panic::catch_unwind(AssertUnwindSafe(unit)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            if !error.is_cancelled() {
                gate.set();
            }
            Err(error)
        }
        Err(payload) => {
            gate.set();
            panic::resume_unwind(payload)
        }
    }
}

/// Wait for a failure or a consumable ordered prefix, then refill the bounded window.
fn wait_window<T, E>(tasks: &[Task<Result<T, E>>]) {
    let signal = Arc::new((Mutex::new(()), Condvar::new()));
    for task in tasks {
        let signal = Arc::clone(&signal);
        task.on_done(move || {
            let _guard = lock(&signal.0);
            signal.1.notify_all();
        });
    }
    let mut guard = lock(&signal.0);
    loop {
        if tasks.iter().any(raised) || tasks.first().is_some_and(Task::done) {
            return;
        }
        // The callbacks notify under the same lock, so no completion is missed between the
        // check and the wait; the timeout only bounds a spurious stall.
        guard = signal
            .1
            .wait_timeout(guard, Duration::from_millis(50))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}
