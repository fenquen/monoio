use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

// thread id begins from 16.
// 0 is default thread
// 1-15 are unused
static ID_GEN: AtomicUsize = AtomicUsize::new(16);

pub(crate) const SPAWN_BLOCKING_THREAD_ID: usize = 0;

pub(crate) fn generateThreadId() -> usize {
    ID_GEN.fetch_add(1, Relaxed)
}

pub(crate) fn get_current_thread_id() -> usize {
    crate::runtime::CURRENT_CONTEXT.with(|ctx| ctx.threadId)
}

pub(crate) fn try_get_current_thread_id() -> Option<usize> {
    crate::runtime::CURRENT_CONTEXT.try_with(|maybe_ctx| maybe_ctx.map(|ctx| ctx.threadId))
}
