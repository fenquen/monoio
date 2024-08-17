#[cfg(feature = "unstable")]
use std::sync::LazyLock;
use std::{sync::Mutex, task::Waker};

use flume::Sender;
use fxhash::FxHashMap;
#[cfg(not(feature = "unstable"))]
use once_cell::sync::Lazy as LazyLock;

use crate::driver::UnparkHandle;

macro_rules! lock {
    ($x: ident) => {
        $x.lock().expect("Unable to lock global map, which is unexpected")
    };
}

static UNPARK: LazyLock<Mutex<FxHashMap<usize, UnparkHandle>>> = LazyLock::new(|| Mutex::new(FxHashMap::default()));

pub(crate) fn register_unpark_handle(threadId: usize, unpark: UnparkHandle) {
    lock!(UNPARK).insert(threadId, unpark);
}

pub(crate) fn unregister_unpark_handle(threadId: usize) {
    lock!(UNPARK).remove(&threadId);
}

pub(crate) fn get_unpark_handle(threadId: usize) -> Option<UnparkHandle> {
    lock!(UNPARK).get(&threadId).cloned()
}

// Global waker sender map
static WAKER_SENDER: LazyLock<Mutex<FxHashMap<usize, Sender<Waker>>>> = LazyLock::new(|| Mutex::new(FxHashMap::default()));

pub(crate) fn register_waker_sender(threadId: usize, sender: Sender<Waker>) {
    lock!(WAKER_SENDER).insert(threadId, sender);
}

pub(crate) fn unregister_waker_sender(threadId: usize) {
    lock!(WAKER_SENDER).remove(&threadId);
}

pub(crate) fn get_waker_sender(threadId: usize) -> Option<Sender<Waker>> {
    lock!(WAKER_SENDER).get(&threadId).cloned()
}
