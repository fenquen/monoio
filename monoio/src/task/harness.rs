use std::{
    future::Future,
    panic,
    ptr::NonNull,
    task::{Context, Poll, Waker},
};
use super::utils::UnsafeCellExt;
use crate::{
    task::{
        core::{Cell, Core, CoreStage, Header, Trailer},
        state::StateSnapshot,
        Schedule, Task,
    },
    utils::thread_id::{try_get_current_thread_id, DEFAULT_THREAD_ID},
};
use crate::task::state::TransitionToNotified;
use crate::task::waker_ref;

pub(crate) struct Harness<T: Future, S: 'static> {
    cell: NonNull<Cell<T, S>>,
}

impl<T: Future, S: 'static> Harness<T, S> {
    pub(crate) unsafe fn fromCellHeaderPtr(cellHeaderPtr: NonNull<Header>) -> Harness<T, S> {
        Harness { cell: cellHeaderPtr.cast::<Cell<T, S>>() }
    }

    fn header(&self) -> &Header {
        unsafe { &self.cell.as_ref().header }
    }

    fn core(&self) -> &Core<T, S> {
        unsafe { &self.cell.as_ref().core }
    }

    fn trailer(&self) -> &Trailer {
        unsafe { &self.cell.as_ref().trailer }
    }
}

impl<T: Future, S: Schedule> Harness<T, S> {
    /// Polls the inner future.
    pub(super) fn poll(self) {
        trace!("MONOIO DEBUG[Harness]:: poll");

        match self.poll_inner() {
            // re-schedule the task
            PollFuture::Notified => {
                self.header().state.ref_inc();
                self.core().scheduler.yield_now(self.buildTask());
            }
            PollFuture::Complete => self.complete(),
            PollFuture::Done => (),
        }
    }

    /// poll_inner does not take a ref-count. We must make sure the task is alive when call this method
    fn poll_inner(&self) -> PollFuture {
        // notified -> running
        self.header().state.transition_to_running();

        // poll the future
        let wakerRef = waker_ref::buildWakerRef::<T, S>(self.header());
        let context = Context::from_waker(&wakerRef);

        if poll_future(&self.core().coreStage, context) == Poll::Ready(()) {
            return PollFuture::Complete;
        }

        use super::state::TransitionToIdle;
        match self.header().state.transition_to_idle() {
            TransitionToIdle::Ok => PollFuture::Done,
            TransitionToIdle::OkNotified => PollFuture::Notified,
        }
    }

    pub(super) fn dealloc(self) {
        trace!("MONOIO DEBUG[Harness]:: dealloc");

        // Release the join waker, if there is one.
        self.trailer().waker.with_mut(drop);

        // Check causality
        self.core().coreStage.with_mut(drop);

        unsafe {
            drop(Box::from_raw(self.cell.as_ptr()));
        }
    }

    #[cfg(feature = "sync")]
    pub(super) fn finish(self, val: <T as Future>::Output) {
        trace!("MONOIO DEBUG[Harness]:: finish");
        self.header().state.transition_to_running();
        self.core().coreStage.store_output(val);
        self.complete();
    }

    // ------------------------------- join handle ---------------------------------

    /// Read the task output into `dst`.
    pub(super) fn try_read_output(self, dst: &mut Poll<T::Output>, waker: &Waker) {
        trace!("MONOIO DEBUG[Harness]:: try_read_output");

        if can_read_output(self.header(), self.trailer(), waker) {
            *dst = Poll::Ready(self.core().coreStage.take_output());
        }
    }

    pub(super) fn drop_join_handle_slow(self) {
        trace!("MONOIO DEBUG[Harness]:: drop_join_handle_slow");

        let mut maybe_panic = None;

        // Try to unset `JOIN_INTEREST`. This must be done as a first step in
        // case the task concurrently completed.
        if self.header().state.unset_join_interested().is_err() {
            // It is our responsibility to drop the output. This is critical as
            // the task output may not be `Send` and as such must remain with
            // the scheduler or `JoinHandle`. i.e. if the output remains in the
            // task structure until the task is deallocated, it may be dropped
            // by a Waker on any arbitrary thread.
            let panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                self.core().coreStage.drop_future_or_output();
            }));

            if let Err(panic) = panic {
                maybe_panic = Some(panic);
            }
        }

        // Drop the `JoinHandle` reference, possibly deallocating the task
        self.drop_reference();

        if let Some(panic) = maybe_panic {
            panic::resume_unwind(panic);
        }
    }

    // ----------------------------------- waker behavior ----------------------------------------

    /// This call consumes a ref-count and notifies the task. This will create a
    /// new Notified and submit it if necessary.
    ///
    /// The caller does not need to hold a ref-count besides the one that was passed to this call.
    pub(super) fn wake_by_val(self) {
        trace!("MONOIO DEBUG[Harness]:: wake_by_val");
        let threadId = self.header().threadId;

        // 如果是其它线程上的
        if is_remote_task(threadId) {
            if self.header().state.transition_to_notified_without_submit() {
                self.drop_reference();
                return;
            }

            // send to target thread
            trace!("MONOIO DEBUG[Harness]:: wake_by_val with another thread id");
            #[cfg(feature = "sync")]
            {
                let rawWaker = waker_ref::buildRawWaker::<T, S>(self.cell.cast::<Header>().as_ptr());
                // # Ref Count: self -> waker
                let waker = unsafe { Waker::from_raw(rawWaker) };

                crate::runtime::CURRENT_CONTEXT.try_with(|context| match context {
                    Some(ctx) => {
                        ctx.send_waker(threadId, waker);
                        ctx.unparkOtherThread(threadId);
                    }
                    None => {
                        let _ = crate::runtime::DEFAULT_CTX.try_with(|default_ctx| {
                            crate::runtime::CURRENT_CONTEXT.set(default_ctx, || {
                                crate::runtime::CURRENT_CONTEXT.with(|ctx| {
                                    ctx.send_waker(threadId, waker);
                                    ctx.unparkOtherThread(threadId);
                                });
                            });
                        });
                    }
                });

                return;
            }

            #[cfg(not(feature = "sync"))]
            {
                panic!("waker can only be sent across threads when `sync` feature enabled");
            }
        }

        match self.header().state.transition_to_notified() {
            // # Ref Count: self -> task
            TransitionToNotified::Submit => self.core().scheduler.schedule(self.buildTask()),
            // # Ref Count: self -> -1
            TransitionToNotified::DoNothing => self.drop_reference(),
        }
    }

    /// This call notifies the task. It will not consume any ref-counts, but the
    /// caller should hold a ref-count.  This will create a new Notified and
    /// submit it if necessary.
    pub(super) fn wake_by_ref(&self) {
        trace!("MONOIO DEBUG[Harness]:: wake_by_ref");
        let threadId = self.header().threadId;

        if is_remote_task(threadId) {
            if self.header().state.transition_to_notified_without_submit() {
                return;
            }

            // send to target thread
            trace!("MONOIO DEBUG[Harness]:: wake_by_ref with another thread id");
            #[cfg(feature = "sync")]
            {
                let waker = waker_ref::buildRawWaker::<T, S>(self.cell.cast::<Header>().as_ptr());
                let waker = unsafe { Waker::from_raw(waker) };
                // We create a new waker so we need to inc ref count.
                self.header().state.ref_inc();

                crate::runtime::CURRENT_CONTEXT.try_with(|maybe_ctx| match maybe_ctx {
                    Some(ctx) => {
                        ctx.send_waker(threadId, waker);
                        ctx.unparkOtherThread(threadId);
                    }
                    None => {
                        let _ = crate::runtime::DEFAULT_CTX.try_with(|default_ctx| {
                            crate::runtime::CURRENT_CONTEXT.set(default_ctx, || {
                                crate::runtime::CURRENT_CONTEXT.with(|ctx| {
                                    ctx.send_waker(threadId, waker);
                                    ctx.unparkOtherThread(threadId);
                                });
                            });
                        });
                    }
                });
                return;
            }

            #[cfg(not(feature = "sync"))]
            {
                panic!("waker can only be sent across threads when `sync` feature enabled");
            }
        }

        use super::state::TransitionToNotified;
        match self.header().state.transition_to_notified() {
            TransitionToNotified::Submit => {
                // # Ref Count: +1 -> task
                self.header().state.ref_inc();
                self.core().scheduler.schedule(self.buildTask());
            }
            TransitionToNotified::DoNothing => (),
        }
    }

    pub(super) fn drop_reference(self) {
        trace!("MONOIO DEBUG[Harness]:: drop_reference");
        if self.header().state.ref_dec() {
            self.dealloc();
        }
    }

    // ----------------------------------- internal ----------------------------------------------

    /// assumes that the state is RUNNING.
    fn complete(self) {
        // The future has completed and its output has been written to the task
        // stage. We transition from running to complete.
        let snapshot = self.header().state.transition_to_complete();

        // We catch panics here in case dropping the future or waking the JoinHandle panics.
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            if !snapshot.is_join_interested() {
                // The `JoinHandle` is not interested in the output of
                // this task. It is our responsibility to drop the output.
                self.core().coreStage.drop_future_or_output();
            } else if snapshot.has_join_waker() {
                // Notify the join handle. The previous transition obtains the lock on the waker cell.
                self.trailer().wake_join();
            }
        }));
    }

    /// Create a new task that holds its own ref-count.
    ///
    /// # Safety
    ///
    /// Any use of `self` after this call must ensure that a ref-count to the
    /// task holds the task alive until after the use of `self`. Passing the
    /// returned Task to any method on `self` is unsound if dropping the Task
    /// could drop `self` before the call on `self` returned.
    fn buildTask(&self) -> Task<S> {
        // safety: The header is at the beginning of the cell, so this cast is safe.
        unsafe { Task::fromHeaderPtr(self.cell.cast()) }
    }
}

/// 是不是其它线程上的
fn is_remote_task(threadId: usize) -> bool {
    if threadId == DEFAULT_THREAD_ID {
        return true;
    }

    match try_get_current_thread_id() {
        Some(currentThreadId) => threadId != currentThreadId,
        None => true,
    }
}

fn can_read_output(header: &Header, trailer: &Trailer, waker: &Waker) -> bool {
    // Load a snapshot of the current task state
    let stateSnapshot = header.state.load();

    debug_assert!(stateSnapshot.is_join_interested());

    if !stateSnapshot.is_complete() {
        // The waker must be stored in the task struct.
        let res = if stateSnapshot.has_join_waker() {
            // There already is a waker stored in the struct. If it matches
            // the provided waker, then there is no further work to do.
            // Otherwise, the waker must be swapped.
            let will_wake = unsafe {
                // Safety: when `JOIN_INTEREST` is set, only `JOIN_HANDLE` may mutate the `waker` field.
                trailer.will_wake(waker)
            };

            // The task is not complete **and** the waker is up to date, there is nothing further that needs to be done.
            if will_wake {
                return false;
            }

            // Unset the `JOIN_WAKER` to gain mutable access to the `waker`
            // field then update the field with the new join worker.
            //
            // This requires two atomic operations, unsetting the bit and
            // then resetting it. If the task transitions to complete
            // concurrently to either one of those operations, then setting
            // the join waker fails and we proceed to reading the task output.
            header.state.unset_waker().and_then(|stateSnapshot| set_join_waker(header, trailer, waker.clone(), stateSnapshot))
        } else {
            set_join_waker(header, trailer, waker.clone(), stateSnapshot)
        };

        match res {
            Ok(_) => return false,
            Err(snapshot) => assert!(snapshot.is_complete())
        }
    }

    true
}

fn set_join_waker(header: &Header,
                  trailer: &Trailer,
                  waker: Waker,
                  stateSnapshot: StateSnapshot) -> Result<StateSnapshot, StateSnapshot> {
    assert!(stateSnapshot.is_join_interested());
    assert!(!stateSnapshot.has_join_waker());

    // Safety: Only the `JoinHandle` may set the `waker` field. When
    // `JOIN_INTEREST` is **not** set, nothing else will touch the field.
    unsafe { trailer.set_waker(Some(waker)); }

    // Update the `JoinWaker` state accordingly
    let res = header.state.set_join_waker();

    // If the state could not be updated, then clear the join waker
    if res.is_err() {
        unsafe { trailer.set_waker(None); }
    }

    res
}

enum PollFuture {
    Complete,
    Notified,
    Done,
}

/// Poll the future. If the future completes, the output is written to the stage field.
fn poll_future<T: Future>(coreStage: &CoreStage<T>, context: Context<'_>) -> Poll<()> {
    // Prepare output for being placed in the core stage.
    let output = match coreStage.poll(context) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(output) => output,
    };

    coreStage.store_output(output);

    Poll::Ready(())
}
