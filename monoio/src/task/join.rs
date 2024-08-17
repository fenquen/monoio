use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use super::raw_task::RawTask;

/// JoinHandle can be used to wait task finished.
/// Note if you drop it directly, task will not be terminated.
pub struct JoinHandle<T> {
    pub rawTask: RawTask,
    pub _p: PhantomData<T>,
}

unsafe impl<T: Send> Send for JoinHandle<T> {}
unsafe impl<T: Send> Sync for JoinHandle<T> {}

impl<T> JoinHandle<T> {
    /// Checks if the task associated with this `JoinHandle` has finished.
    pub fn is_finished(&self) -> bool {
        let state = self.rawTask.header().state.load();
        state.is_complete()
    }
}

impl<T> Unpin for JoinHandle<T> {}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut ret = Poll::Pending;

        // Try to read the task output. If the task is not yet complete, the
        // waker is stored and is notified once the task does complete.
        //
        // The function must go via the vtable, which requires erasing generic
        // types. To do this, the function "return" is placed on the stack
        // **before** calling the function and is passed into the function using
        // `*mut ()`.
        //
        // Safety:
        //
        // The type of `T` must match the task's output type.
        unsafe {
            self.rawTask.try_read_output(&mut ret as *mut _ as *mut (), cx.waker());
        }

        ret
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.rawTask.header().state.drop_join_handle_fast().is_ok() {
            return;
        }

        self.rawTask.drop_join_handle_slow();
    }
}
