use std::{
    future::Future,
    ptr::NonNull,
    task::{Poll, Waker},
};

use crate::task::{Cell, Harness, Header, Schedule};

pub(crate) struct RawTask {
    headerPtr: NonNull<Header>,
}

impl Clone for RawTask {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for RawTask {}

pub(crate) struct Vtable {
    /// Poll the future
    pub(crate) poll: unsafe fn(NonNull<Header>),

    /// Deallocate the memory
    pub(crate) dealloc: unsafe fn(NonNull<Header>),

    /// Read the task output, if complete
    pub(crate) try_read_output: unsafe fn(NonNull<Header>, *mut (), &Waker),

    /// The join handle has been dropped
    pub(crate) drop_join_handle_slow: unsafe fn(NonNull<Header>),

    /// Set future output
    #[cfg(feature = "sync")]
    pub(crate) finish: unsafe fn(NonNull<Header>, *mut ()),
}

/// Get the vtable for the requested `T` and `S` generics.
pub(super) fn vtable<T: Future, S: Schedule>() -> &'static Vtable {
    &Vtable {
        poll: poll::<T, S>,
        dealloc: dealloc::<T, S>,
        try_read_output: try_read_output::<T, S>,
        drop_join_handle_slow: drop_join_handle_slow::<T, S>,
        #[cfg(feature = "sync")]
        finish: finish::<T, S>,
    }
}

impl RawTask {
    pub(crate) fn new<T: Future, S: Schedule>(threadId: usize, future: T, scheduler: S) -> RawTask {
        let cellPtr = Box::into_raw(Cell::boxNew(threadId, future, scheduler));
        let headerPtr = unsafe { NonNull::new_unchecked(cellPtr as *mut Header) };

        RawTask { headerPtr }
    }

    pub(crate) unsafe fn fromHeaderPtr(ptr: NonNull<Header>) -> RawTask {
        RawTask { headerPtr: ptr }
    }

    pub(crate) fn header(&self) -> &Header {
        unsafe { self.headerPtr.as_ref() }
    }

    /// Safety: mutual exclusion is required to call this function.
    pub(crate) fn poll(self) {
        unsafe { (self.header().vtable.poll)(self.headerPtr) }
    }

    pub(crate) fn dealloc(self) {
        unsafe { (self.header().vtable.dealloc)(self.headerPtr) }
    }

    /// Safety: `dst` must be a `*mut Poll<super::Result<T::Output>>` where `T` is the future stored by the task.
    pub(crate) unsafe fn try_read_output(self, dst: *mut (), waker: &Waker) {
        (self.header().vtable.try_read_output)(self.headerPtr, dst, waker);
    }

    pub(crate) fn drop_join_handle_slow(self) {
        unsafe { (self.header().vtable.drop_join_handle_slow)(self.headerPtr) }
    }

    #[cfg(feature = "sync")]
    pub(crate) unsafe fn finish(self, val_slot: *mut ()) {
        unsafe { (self.header().vtable.finish)(self.headerPtr, val_slot) }
    }
}

unsafe fn poll<T: Future, S: Schedule>(cellHeaderPtr: NonNull<Header>) {
    Harness::<T, S>::fromCellHeaderPtr(cellHeaderPtr).poll()
}

unsafe fn dealloc<T: Future, S: Schedule>(cellHeaderPtr: NonNull<Header>) {
    Harness::<T, S>::fromCellHeaderPtr(cellHeaderPtr).dealloc();
}

unsafe fn try_read_output<T: Future, S: Schedule>(cellHeaderPtr: NonNull<Header>,
                                                  dst: *mut (),
                                                  waker: &Waker) {
    let dst = &mut *(dst as *mut Poll<T::Output>);
    Harness::<T, S>::fromCellHeaderPtr(cellHeaderPtr).try_read_output(dst, waker);
}

unsafe fn drop_join_handle_slow<T: Future, S: Schedule>(cellHeaderPtr: NonNull<Header>) {
    Harness::<T, S>::fromCellHeaderPtr(cellHeaderPtr).drop_join_handle_slow()
}

#[cfg(feature = "sync")]
unsafe fn finish<T: Future, S: Schedule>(cellHeaderPtr: NonNull<Header>, val: *mut ()) {
    let harness = Harness::<T, S>::fromCellHeaderPtr(cellHeaderPtr);
    let val = &mut *(val as *mut Option<<T as Future>::Output>);
    harness.finish(val.take().unwrap());
}
