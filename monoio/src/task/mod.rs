//! Task impl
// Heavily borrowed from tokio.
// Copyright (c) 2021 Tokio Contributors, licensed under the MIT license.

mod utils;
pub(crate) mod waker_fn;

mod core;
use self::core::{Cell, Header};

mod harness;
use self::harness::Harness;

mod join;
#[allow(unreachable_pub)] // https://github.com/rust-lang/rust/issues/57411
pub use self::join::JoinHandle;

mod raw_task;
use self::raw_task::RawTask;

mod state;
mod waker_ref;

use std::{future::Future, marker::PhantomData, ptr::NonNull};

/// An owned handle to the task, tracked by ref count, not sendable
#[repr(transparent)]
pub(crate) struct Task<S: 'static> {
    rawTask: RawTask,
    _p: PhantomData<S>,
}

impl<S: 'static> Task<S> {
    unsafe fn fromHeaderPtr(ptr: NonNull<Header>) -> Task<S> {
        Task {
            rawTask: RawTask::fromHeaderPtr(ptr),
            _p: PhantomData,
        }
    }

    pub(crate) fn run(self) {
        self.rawTask.poll();
    }

    #[cfg(feature = "sync")]
    pub(crate) unsafe fn finish(&mut self, val_slot: *mut ()) {
        self.rawTask.finish(val_slot);
    }
}

impl<S: 'static> Drop for Task<S> {
    fn drop(&mut self) {
        // Deallocate if this is the final ref count
        if self.rawTask.header().state.ref_dec() {
            self.rawTask.dealloc();
        }
    }
}

pub(crate) trait Schedule: Sized + 'static {
    /// 塞到context的taskQueue
    fn schedule(&self, task: Task<Self>);

    /// Schedule the task to run in the near future, yielding the thread to other tasks.
    fn yield_now(&self, task: Task<Self>) {
        self.schedule(task);
    }
}

pub(crate) fn newTask<T, S>(threadId: usize,
                            future: T,
                            scheduler: S) -> (Task<S>, JoinHandle<T::Output>)
where
    S: Schedule,
    T: Future + 'static,
    T::Output: 'static,
{
    unsafe { new_task_holding(threadId, future, scheduler) }
}

pub(crate) unsafe fn new_task_holding<T:Future, S:Schedule>(threadId: usize,
                                            future: T,
                                            scheduler: S) -> (Task<S>, JoinHandle<T::Output>) {
    let rawTask = RawTask::new::<T, S>(threadId, future, scheduler);

    let task = Task {
        rawTask,
        _p: PhantomData,
    };

    let join = JoinHandle {
        rawTask,
        _p: PhantomData,
    };

    (task, join)
}
