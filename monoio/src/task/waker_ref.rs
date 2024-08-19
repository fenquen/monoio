use std::{
    future::Future,
    marker::PhantomData,
    mem::ManuallyDrop,
    ptr::NonNull,
    task::{RawWaker, RawWakerVTable, Waker},
};
use std::ops::Deref;
use super::{core::Header, harness::Harness, Schedule};

pub(super) struct WakerRef<'a, S: 'static> {
    waker: ManuallyDrop<Waker>,
    _p: PhantomData<(&'a Header, S)>,
}

/// Returns a `WakerRef` which avoids having to pre-emptively increase the refcount if there is no need to do so.
pub(super) fn buildWakerRef<T: Future, S: Schedule>(header: &Header) -> WakerRef<'_, S> {
    // `Waker::will_wake` uses the VTABLE pointer as part of the check.
    // This means that `will_wake` will always return false when using the current task's waker. (https://github.com/rust-lang/rust/issues/66281).
    //
    // To fix this, we use a single vtable. Since we pass in a reference at this point and not an *owned* waker,
    // we must ensure that `drop` is never called on this waker instance.
    // This is done by wrapping it with `ManuallyDrop` and then never calling drop.
    let waker = unsafe { ManuallyDrop::new(Waker::from_raw(buildRawWaker::<T, S>(header))) };

    WakerRef {
        waker,
        _p: PhantomData,
    }
}

impl<S> Deref for WakerRef<'_, S> {
    type Target = Waker;

    fn deref(&self) -> &Waker {
        &self.waker
    }
}

pub(super) fn buildRawWaker<T: Future, S: Schedule>(headerPtr: *const Header) -> RawWaker {
    let rawWakerVTable =
        &RawWakerVTable::new(
            clone::<T, S>,
            wake_by_val::<T, S>,
            wake_by_ref::<T, S>,
            drop_waker::<T, S>,
        );

    RawWaker::new(headerPtr as *const (), rawWakerVTable)
}

unsafe fn clone<T: Future, S: Schedule>(ptr: *const ()) -> RawWaker {
    let headerPtr = ptr as *const Header;
    (*headerPtr).state.ref_inc();

    buildRawWaker::<T, S>(headerPtr)
}

unsafe fn wake_by_val<T: Future, S: Schedule>(ptr: *const ()) {
    let headerPtr = NonNull::new_unchecked(ptr as *mut Header);
    let harness = Harness::<T, S>::fromCellHeaderPtr(headerPtr);

    harness.wake_by_val();
}

// Wake without consuming the waker
unsafe fn wake_by_ref<T: Future, S: Schedule>(ptr: *const ()) {
    let headerPtr = NonNull::new_unchecked(ptr as *mut Header);
    let harness = Harness::<T, S>::fromCellHeaderPtr(headerPtr);

    harness.wake_by_ref();
}

unsafe fn drop_waker<T: Future, S: Schedule>(ptr: *const ()) {
    let headerPtr = NonNull::new_unchecked(ptr as *mut Header);
    let harness = Harness::<T, S>::fromCellHeaderPtr(headerPtr);

    harness.drop_reference();
}