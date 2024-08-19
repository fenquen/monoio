use std::{cell::UnsafeCell, future::Future, mem, pin::Pin, task::{Context, Poll, Waker}};

use super::{
    raw_task::{self, RawTaskVtable},
    state::State,
    utils::UnsafeCellExt,
    Schedule,
};

#[repr(C)]
pub(crate) struct Cell<T: Future, S> {
    pub(crate) header: Header,
    pub(crate) core: Core<T, S>,
    pub(crate) trailer: Trailer,
}

impl<T: Future, S: Schedule> Cell<T, S> {
    /// Allocates a new task cell, containing the header, trailer, and core structures.
    pub(crate) fn boxNew(threadId: usize, future: T, scheduler: S) -> Box<Cell<T, S>> {
        Box::new(Cell {
            header: Header {
                state: State::new(),
                rawTaskVtable: raw_task::buildRawTaskVtable::<T, S>(),
                threadId,
            },
            core: Core {
                scheduler,
                coreStage: CoreStage {
                    stage: UnsafeCell::new(Stage::Running(future)),
                },
            },
            trailer: Trailer {
                waker: UnsafeCell::new(None),
            },
        })
    }
}

#[repr(C)]
pub(crate) struct Header {
    pub(crate) state: State,

    /// Table of function pointers for executing actions on the task.
    pub(crate) rawTaskVtable: &'static RawTaskVtable,

    /// Thread ID(sync: used for wake task on its thread; sync disabled: do checking)
    pub(crate) threadId: usize,
}

impl Header {
    #[allow(unused)]
    pub(crate) fn getThreadId(&self) -> usize {
        // safety: If there are concurrent writes, then that write has violated the safety requirements on `set_owner_id`.
        self.threadId
    }
}

pub(crate) struct Core<T: Future, S> {
    /// Scheduler used to drive this future
    pub(crate) scheduler: S,

    /// Either the future or the output
    pub(crate) coreStage: CoreStage<T>,
}

pub(crate) struct CoreStage<T: Future> {
    stage: UnsafeCell<Stage<T>>,
}

pub(crate) enum Stage<T: Future> {
    Running(T),
    Finished(T::Output),
    Consumed,
}

impl<T: Future> CoreStage<T> {
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut Stage<T>) -> R) -> R {
        self.stage.with_mut(f)
    }

    pub(crate) fn poll(&self, mut context: Context<'_>) -> Poll<T::Output> {
        let res = {
            self.with_mut(|stagePtr| {
                // Safety: The caller ensures mutual exclusion to the field.
                let future =
                    match unsafe { &mut *stagePtr } {
                        Stage::Running(future) => future,
                        _ => unreachable!("unexpected stage"),
                    };

                // Safety: The caller ensures the future is pinned.
                let future = unsafe { Pin::new_unchecked(future) };
                future.poll(&mut context)
            })
        };

        if res.is_ready() {
            self.drop_future_or_output();
        }

        res
    }

    /// Drop the future
    ///
    /// # Safety
    ///
    /// The caller must ensure it is safe to mutate the `stage` field.
    pub(crate) fn drop_future_or_output(&self) {
        // Safety: the caller ensures mutual exclusion to the field.
        unsafe {
            self.set_stage(Stage::Consumed);
        }
    }

    /// Store the task output
    ///
    /// # Safety
    ///
    /// The caller must ensure it is safe to mutate the `stage` field.
    pub(crate) fn store_output(&self, output: T::Output) {
        // Safety: the caller ensures mutual exclusion to the field.
        unsafe {
            self.set_stage(Stage::Finished(output));
        }
    }

    /// Take the task output
    ///
    /// # Safety
    ///
    /// The caller must ensure it is safe to mutate the `stage` field.
    pub(crate) fn take_output(&self) -> T::Output {
        self.with_mut(|stagePtr| {
            match mem::replace(unsafe { &mut *stagePtr }, Stage::Consumed) {
                Stage::Finished(output) => output,
                _ => panic!("JoinHandle polled after completion"),
            }
        })
    }

    unsafe fn set_stage(&self, stage: Stage<T>) {
        self.with_mut(|ptr| *ptr = stage)
    }
}

pub(crate) struct Trailer {
    /// Consumer task waiting on completion of this task.
    pub(crate) waker: UnsafeCell<Option<Waker>>,
}

impl Trailer {
    pub(crate) unsafe fn set_waker(&self, waker: Option<Waker>) {
        self.waker.with_mut(|ptr| {
            *ptr = waker;
        });
    }

    pub(crate) unsafe fn will_wake(&self, waker: &Waker) -> bool {
        self.waker.with(|wakerPtr| (*wakerPtr).as_ref().unwrap().will_wake(waker))
    }

    pub(crate) fn wake_join(&self) {
        self.waker.with(|wakerPtr| match unsafe { &*wakerPtr } {
            Some(waker) => waker.wake_by_ref(),
            None => panic!("waker missing"),
        });
    }
}
