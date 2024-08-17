/// Monoio Driver.
#[allow(dead_code)]
pub(crate) mod op;
#[cfg(all(feature = "poll-io", unix))]
pub(crate) mod poll;
#[cfg(any(feature = "legacy", feature = "poll-io"))]
pub(crate) mod ready;
#[cfg(any(feature = "legacy", feature = "poll-io"))]
pub(crate) mod scheduled_io;
#[allow(dead_code)]
pub(crate) mod shared_fd;
#[cfg(feature = "sync")]
pub(crate) mod thread;

#[cfg(feature = "legacy")]
mod legacy;
#[cfg(all(target_os = "linux", feature = "iouring"))]
mod uring;

mod util;

use alloc::rc::Rc;
use std::{
    io,
    task::{Context, Poll},
    time::Duration,
};
use std::cell::UnsafeCell;
#[allow(unreachable_pub)]
#[cfg(feature = "legacy")]
pub use self::legacy::LegacyDriver;
#[cfg(feature = "legacy")]
use self::legacy::LegacyDriverInner;
use self::op::{CompletionMeta, Op, OpAble};
#[cfg(all(target_os = "linux", feature = "iouring"))]
pub use self::uring::IoUringDriver;
#[cfg(all(target_os = "linux", feature = "iouring"))]
use self::uring::UringInner;

/// Unpark a runtime of another thread.
pub(crate) mod unpark {
    #[allow(unreachable_pub)]
    pub trait Unpark: Sync + Send + 'static {
        /// Unblocks a thread that is blocked by the associated `Park` handle.
        ///
        /// Calling `unpark` atomically makes available the unpark token, if it
        /// is not already available.
        ///
        /// # Panics
        ///
        /// This function **should** not panic, but ultimately, panics are left
        /// as an implementation detail. Refer to the documentation for
        /// the specific `Unpark` implementation
        fn unpark(&self) -> std::io::Result<()>;
    }
}

impl unpark::Unpark for Box<dyn unpark::Unpark> {
    fn unpark(&self) -> io::Result<()> {
        (**self).unpark()
    }
}

impl unpark::Unpark for std::sync::Arc<dyn unpark::Unpark> {
    fn unpark(&self) -> io::Result<()> {
        (**self).unpark()
    }
}

/// Core driver trait.
pub trait Driver {
    /// Run with driver TLS.
    fn with<R>(&self, f: impl FnOnce() -> R) -> R;

    /// Submit ops to kernel and process returned events.
    fn submit(&self) -> io::Result<()>;

    /// Wait infinitely and process returned events.
    fn park(&self) -> io::Result<()>;

    /// Wait with timeout and process returned events.
    fn park_timeout(&self, duration: Duration) -> io::Result<()>;

    /// The struct to wake thread from another.
    #[cfg(feature = "sync")]
    type Unpark: unpark::Unpark;

    #[cfg(feature = "sync")]
    fn getUnpark(&self) -> Self::Unpark;
}

scoped_thread_local!(pub(crate) static CURRENT_INNER: DriverInner);

#[derive(Clone)]
pub(crate) enum DriverInner {
    #[cfg(all(target_os = "linux", feature = "iouring"))]
    Uring(Rc<UnsafeCell<UringInner>>),

    #[cfg(feature = "legacy")]
    Legacy(Rc<UnsafeCell<LegacyDriverInner>>),
}

impl DriverInner {
    fn submit_with(&self, opAble: impl OpAble) -> io::Result<Op<impl OpAble>> {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::submit_with_data(this, opAble),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(legacyInner) => LegacyDriverInner::submitOpAble(legacyInner, opAble),
            #[cfg(all(not(feature = "legacy"), not(all(target_os = "linux", feature = "iouring"))))]
            _ => { util::feature_panic(); }
        }
    }

    #[allow(unused)]
    fn pollOpAble<T: OpAble>(&self,
                             opAble: &mut T,
                             index: usize,
                             cx: &mut Context<'_>) -> Poll<CompletionMeta> {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::poll_op(this, index, cx),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(legacyInner) => LegacyDriverInner::pollOpAble::<T>(legacyInner, opAble, cx),
            #[cfg(all(not(feature = "legacy"), not(all(target_os = "linux", feature = "iouring"))))]
            _ => { util::feature_panic();}
        }
    }

    #[cfg(feature = "poll-io")]
    fn poll_legacy_op<T: OpAble>(
        &self,
        data: &mut T,
        cx: &mut Context<'_>,
    ) -> Poll<CompletionMeta> {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::poll_legacy_op(this, data, cx),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(legacyInner) => LegacyDriverInner::pollOpAble::<T>(legacyInner, data, cx),
            #[cfg(all(
                not(feature = "legacy"),
                not(all(target_os = "linux", feature = "iouring"))
            ))]
            _ => {
                util::feature_panic();
            }
        }
    }

    #[allow(unused)]
    fn drop_op<T: 'static>(&self, index: usize, data: &mut Option<T>) {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::drop_op(this, index, data),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(_) => {}
            #[cfg(all(
                not(feature = "legacy"),
                not(all(target_os = "linux", feature = "iouring"))
            ))]
            _ => {
                util::feature_panic();
            }
        }
    }

    #[allow(unused)]
    pub(super) unsafe fn cancel_op(&self, op_canceller: &op::OpCanceller) {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::cancel_op(this, op_canceller.index),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(this) => {
                if let Some(direction) = op_canceller.direction {
                    LegacyDriverInner::cancel_op(this, op_canceller.index, direction)
                }
            }
            #[cfg(all(
                not(feature = "legacy"),
                not(all(target_os = "linux", feature = "iouring"))
            ))]
            _ => {
                util::feature_panic();
            }
        }
    }

    #[cfg(all(target_os = "linux", feature = "iouring", feature = "legacy"))]
    fn is_legacy(&self) -> bool {
        matches!(self, Inner::Legacy(..))
    }

    #[cfg(all(target_os = "linux", feature = "iouring", not(feature = "legacy")))]
    fn is_legacy(&self) -> bool {
        false
    }

    #[allow(unused)]
    #[cfg(not(all(target_os = "linux", feature = "iouring")))]
    fn is_legacy(&self) -> bool {
        true
    }
}

#[cfg(feature = "sync")]
#[derive(Clone)]
pub(crate) enum UnparkHandle {
    #[cfg(all(target_os = "linux", feature = "iouring"))]
    Uring(self::uring::UringUnpark),

    #[cfg(feature = "legacy")]
    Legacy(legacy::LegacyUnpark),
}

#[cfg(feature = "sync")]
impl unpark::Unpark for UnparkHandle {
    fn unpark(&self) -> io::Result<()> {
        match self {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            UnparkHandle::Uring(inner) => inner.unpark(),
            #[cfg(feature = "legacy")]
            UnparkHandle::Legacy(unparkHandle) => unparkHandle.unpark(),
            #[cfg(all(not(feature = "legacy"), not(all(target_os = "linux", feature = "iouring"))))]
            _ => { util::feature_panic();}
        }
    }
}

#[cfg(all(feature = "sync", target_os = "linux", feature = "iouring"))]
impl From<self::uring::UringUnpark> for UnparkHandle {
    fn from(inner: self::uring::UringUnpark) -> Self {
        Self::Uring(inner)
    }
}

#[cfg(all(feature = "sync", feature = "legacy"))]
impl From<legacy::LegacyUnpark> for UnparkHandle {
    fn from(inner: legacy::LegacyUnpark) -> Self {
        Self::Legacy(inner)
    }
}

#[cfg(feature = "sync")]
impl UnparkHandle {
    #[allow(unused)]
    pub(crate) fn current() -> Self {
        CURRENT_INNER.with(|inner| match inner {
            #[cfg(all(target_os = "linux", feature = "iouring"))]
            DriverInner::Uring(this) => UringInner::unpark(this).into(),
            #[cfg(feature = "legacy")]
            DriverInner::Legacy(this) => LegacyDriverInner::getLegacyUnpark(this).into(),
        })
    }
}
