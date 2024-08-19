use std::cell::RefCell;
use std::future::Future;
use std::task::Poll;
use fxhash::FxHashMap;
#[cfg(any(all(target_os = "linux", feature = "iouring"), feature = "legacy"))]
use crate::time::TimeDriver;
#[cfg(all(target_os = "linux", feature = "iouring"))]
use crate::IoUringDriver;
#[cfg(feature = "legacy")]
use crate::LegacyDriver;
use crate::{
    driver::Driver,
    scheduler::{LocalScheduler, TaskQueue},
    task::{JoinHandle},
    time::driver::Handle as TimeHandle,
};
use crate::blocking::BlockingHandle;
use crate::task::waker_fn;
use crate::utils::thread_id;

#[cfg(feature = "sync")]
thread_local! {
    pub(crate) static DEFAULT_CTX: Context = Context {
        threadId: thread_id::SPAWN_BLOCKING_THREAD_ID,
        threadId_unpark: RefCell::new(fxhash::FxHashMap::default()),
        threadId_wakerSender: RefCell::new(fxhash::FxHashMap::default()),
        taskQueue: Default::default(),
        time_handle: None,
        blocking_handle: BlockingHandle::Empty(crate::blocking::BlockingStrategy::Panic),
    };
}

scoped_thread_local!(pub(crate) static CURRENT_CONTEXT: Context);

pub(crate) struct Context {
    /// 被调用了wake()的
    pub(crate) taskQueue: TaskQueue,

    /// Thread id(not the kernel thread id but a generated unique number)
    pub(crate) threadId: usize,

    /// Thread unpark handles
    #[cfg(feature = "sync")]
    pub(crate) threadId_unpark: RefCell<FxHashMap<usize, crate::driver::UnparkHandle>>,

    /// Waker sender cache
    #[cfg(feature = "sync")]
    pub(crate) threadId_wakerSender: RefCell<FxHashMap<usize, flume::Sender<std::task::Waker>>>,

    /// Time Handle
    pub(crate) time_handle: Option<TimeHandle>,

    /// Blocking Handle
    #[cfg(feature = "sync")]
    pub(crate) blocking_handle: BlockingHandle,
}

impl Context {
    #[cfg(feature = "sync")]
    pub(crate) fn new(blocking_handle: BlockingHandle) -> Context {
        let threadId = crate::builder::BUILD_THREAD_ID.with(|id| *id);

        Context {
            threadId,
            threadId_unpark: RefCell::new(FxHashMap::default()),
            threadId_wakerSender: RefCell::new(FxHashMap::default()),
            taskQueue: TaskQueue::default(),
            time_handle: None,
            blocking_handle,
        }
    }

    #[cfg(not(feature = "sync"))]
    pub(crate) fn new() -> Self {
        let thread_id = crate::builder::BUILD_THREAD_ID.with(|id| *id);

        Self {
            threadId: thread_id,
            taskQueue: TaskQueue::default(),
            time_handle: None,
        }
    }

    #[allow(unused)]
    #[cfg(feature = "sync")]
    pub(crate) fn unparkOtherThread(&self, threadId: usize) {
        use crate::driver::{unpark::Unpark};

        if let Some(unparkHandle) = self.threadId_unpark.borrow().get(&threadId) {
            unparkHandle.unpark();
            return;
        }

        if let Some(unparkHandle) = crate::driver::thread::get_unpark_handle(threadId) {
            // Write back to local cache
            self.threadId_unpark.borrow_mut().insert(threadId, unparkHandle.clone());
            unparkHandle.unpark();
        }
    }

    #[allow(unused)]
    #[cfg(feature = "sync")]
    pub(crate) fn send_waker(&self, threadId: usize, waker: std::task::Waker) {
        if let Some(wakerSender) = self.threadId_wakerSender.borrow().get(&threadId) {
            let _ = wakerSender.send(waker);
            return;
        }

        if let Some(wakerSender) = crate::driver::thread::get_waker_sender(threadId) {
            let _ = wakerSender.send(waker);
            self.threadId_wakerSender.borrow_mut().insert(threadId, wakerSender);
        }
    }
}

/// Monoio runtime
pub struct Runtime<D> {
    pub(crate) context: Context,
    pub(crate) driver: D,
}

impl<D> Runtime<D> {
    pub(crate) fn new(context: Context, driver: D) -> Runtime<D> {
        Runtime { context, driver }
    }

    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
        D: Driver,
    {
        assert!(!CURRENT_CONTEXT.is_set(), "Can not start a runtime inside a runtime");

        let dummyWaker = waker_fn::buildDummyWaker();
        let context = &mut std::task::Context::from_waker(&dummyWaker);

        self.driver.with(|| {
            CURRENT_CONTEXT.set(&self.context, || {
                #[cfg(feature = "sync")]
                let join = unsafe { spawn_without_static(future) };

                #[cfg(not(feature = "sync"))]
                let join = future;

                let mut join = std::pin::pin!(join);

                waker_fn::setShouldPoll();

                loop {
                    loop {
                        // 应对调用其它之前调用spawn的
                        // consume all tasks(with max round to prevent io starvation)
                        let mut max_round = self.context.taskQueue.len() * 2;
                        while let Some(task) = self.context.taskQueue.pop() {
                            // 会调用future的poll()
                            task.run();

                            // maybe there's a looping task
                            if max_round == 0 {
                                break;
                            }

                            max_round -= 1;
                        }

                        // 应对当前的future
                        while waker_fn::readShouldPoll() {
                            if let Poll::Ready(t) = join.as_mut().poll(context) {
                                return t;
                            }
                        }

                        // no task to execute, we should wait for io blockingly Hot path
                        if self.context.taskQueue.is_empty() {
                            break;
                        }

                        // Cold path
                        let _ = self.driver.submit();
                    }

                    // Wait and Process CQ(the error is ignored for not debug mode)
                    #[cfg(not(all(debug_assertions, feature = "debug")))]
                    let _ = self.driver.park();

                    // 应对read的
                    #[cfg(all(debug_assertions, feature = "debug"))]
                    if let Err(e) = self.driver.park() {
                        trace!("park error: {:?}", e);
                    }
                }
            })
        })
    }
}

/// Fusion Runtime is a wrapper of io_uring driver or legacy driver based runtime.
#[cfg(feature = "legacy")]
pub enum FusionRuntime<#[cfg(all(target_os = "linux", feature = "iouring"))] L, R> {
    /// Uring driver based runtime.
    #[cfg(all(target_os = "linux", feature = "iouring"))]
    Uring(Runtime<L>),
    /// Legacy driver based runtime.
    Legacy(Runtime<R>),
}

/// Fusion Runtime is a wrapper of io_uring driver or legacy driver based
/// runtime.
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
pub enum FusionRuntime<L> {
    /// Uring driver based runtime.
    Uring(Runtime<L>),
}

#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl<L, R> FusionRuntime<L, R>
where
    L: Driver,
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Uring(inner) => {
                info!("Monoio is running with io_uring driver");
                inner.block_on(future)
            }
            FusionRuntime::Legacy(inner) => {
                info!("Monoio is running with legacy driver");
                inner.block_on(future)
            }
        }
    }
}

#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl<R> FusionRuntime<R>
where
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Legacy(inner) => inner.block_on(future),
        }
    }
}

#[cfg(all(not(feature = "legacy"), all(target_os = "linux", feature = "iouring")))]
impl<R> FusionRuntime<R>
where
    R: Driver,
{
    /// Block on
    pub fn block_on<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        match self {
            FusionRuntime::Uring(inner) => inner.block_on(future),
        }
    }
}

// L -> Fusion<L, R>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<IoUringDriver>> for FusionRuntime<IoUringDriver, LegacyDriver> {
    fn from(r: Runtime<IoUringDriver>) -> Self {
        Self::Uring(r)
    }
}

// TL -> Fusion<TL, TR>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<TimeDriver<IoUringDriver>>>
for FusionRuntime<TimeDriver<IoUringDriver>, TimeDriver<LegacyDriver>>
{
    fn from(r: Runtime<TimeDriver<IoUringDriver>>) -> Self {
        Self::Uring(r)
    }
}

// R -> Fusion<L, R>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<LegacyDriver>> for FusionRuntime<IoUringDriver, LegacyDriver> {
    fn from(r: Runtime<LegacyDriver>) -> Self {
        Self::Legacy(r)
    }
}

// TR -> Fusion<TL, TR>
#[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
impl From<Runtime<TimeDriver<LegacyDriver>>>
for FusionRuntime<TimeDriver<IoUringDriver>, TimeDriver<LegacyDriver>>
{
    fn from(r: Runtime<TimeDriver<LegacyDriver>>) -> Self {
        Self::Legacy(r)
    }
}

// R -> Fusion<R>
#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl From<Runtime<LegacyDriver>> for FusionRuntime<LegacyDriver> {
    fn from(r: Runtime<LegacyDriver>) -> Self {
        Self::Legacy(r)
    }
}

// TR -> Fusion<TR>
#[cfg(all(feature = "legacy", not(all(target_os = "linux", feature = "iouring"))))]
impl From<Runtime<TimeDriver<LegacyDriver>>> for FusionRuntime<TimeDriver<LegacyDriver>> {
    fn from(r: Runtime<TimeDriver<LegacyDriver>>) -> Self {
        Self::Legacy(r)
    }
}

// L -> Fusion<L>
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
impl From<Runtime<IoUringDriver>> for FusionRuntime<IoUringDriver> {
    fn from(r: Runtime<IoUringDriver>) -> Self {
        Self::Uring(r)
    }
}

// TL -> Fusion<TL>
#[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
impl From<Runtime<TimeDriver<IoUringDriver>>> for FusionRuntime<TimeDriver<IoUringDriver>> {
    fn from(r: Runtime<TimeDriver<IoUringDriver>>) -> Self {
        Self::Uring(r)
    }
}

/// Spawns a new asynchronous task, returning a [`JoinHandle`] for it.
///
/// Spawning a task enables the task to execute concurrently to other tasks.
/// There is no guarantee that a spawned task will execute to completion. When a
/// runtime is shutdown, all outstanding tasks are dropped, regardless of the
/// lifecycle of that task.
///
///
/// [`JoinHandle`]: monoio::task::JoinHandle
///
/// ```
/// #[monoio::main]
/// async fn main() {
///     let handle = monoio::spawn(async {
///         println!("hello from a background task");
///     });
///
///     // Let the task complete
///     handle.await;
/// }
/// ```
pub fn spawn<T: Future<Output: 'static> + 'static>(future: T) -> JoinHandle<T::Output> {
    let (task, join) =
        crate::task::newTask(thread_id::get_current_thread_id(), future, LocalScheduler);

    CURRENT_CONTEXT.with(|ctx| ctx.taskQueue.push(task));

    join
}

#[cfg(feature = "sync")]
unsafe fn spawn_without_static<T: Future>(future: T) -> JoinHandle<T::Output> {
    let (task, join) =
        crate::task::new_task_holding(thread_id::get_current_thread_id(), future, LocalScheduler);

    CURRENT_CONTEXT.with(|ctx| { ctx.taskQueue.push(task); });

    join
}