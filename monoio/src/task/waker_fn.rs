use core::task::{RawWaker, RawWakerVTable, Waker};
use std::cell::Cell;
use std::ptr;

/// create a waker that does nothing.
///
/// this `Waker` is useful for polling a `Future` to check whether it is `Ready`, without doing any additional work.
pub(crate) fn buildDummyWaker() -> Waker {
    fn buildDummyRawWaker() -> RawWaker {
        // the pointer is never used, so null is ok
        RawWaker::new(ptr::null::<()>(), buildDummyRawWakerVTable())
    }

    fn buildDummyRawWakerVTable() -> &'static RawWakerVTable {
        &RawWakerVTable::new(
            |_| buildDummyRawWaker(),
            |_| setShouldPoll(),
            |_| setShouldPoll(),
            |_| {},
        )
    }

    unsafe { Waker::from_raw(buildDummyRawWaker()) }
}

#[cfg(feature = "unstable")]
#[thread_local]
static SHOULD_POLL: Cell<bool> = Cell::new(true);

#[cfg(not(feature = "unstable"))]
thread_local! {
    static SHOULD_POLL: Cell<bool> = const { Cell::new(true) };
}

#[inline]
pub(crate) fn readShouldPoll() -> bool {
    SHOULD_POLL.replace(false)
}

#[inline]
pub(crate) fn setShouldPoll() {
    SHOULD_POLL.set(true);
}
