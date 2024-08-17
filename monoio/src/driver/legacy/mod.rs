//! Monoio Legacy Driver.

use std::{
    cell::UnsafeCell,
    io,
    rc::Rc,
    task::{Context, Poll},
    time::Duration,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use super::{
    op::{CompletionMeta, Op, OpAble},
    ready::{self, Ready},
    scheduled_io::ScheduledIo,
    Driver, DriverInner, CURRENT_INNER,
};
use crate::utils::slab::Slab;

#[cfg(feature = "sync")]
mod waker;
#[cfg(feature = "sync")]
pub(crate) use waker::LegacyUnpark;
use crate::driver::legacy::waker::MioWakerWrapper;

/// Driver with Poll-like syscall.
#[allow(unreachable_pub)]
pub struct LegacyDriver {
    legacyDriverInner: Rc<UnsafeCell<LegacyDriverInner>>,

    // Used for drop
    #[cfg(feature = "sync")]
    threadId: usize,
}

pub(crate) struct LegacyDriverInner {
    pub(crate) io_dispatch: Slab<ScheduledIo>,

    #[cfg(unix)]
    mioEvents: mio::Events,

    #[cfg(unix)]
    mioPoll: mio::Poll,

    #[cfg(feature = "sync")]
    mioWakerWrapper: Arc<MioWakerWrapper>,

    #[cfg(feature = "sync")]
    wakerReceiver: flume::Receiver<std::task::Waker>,
}

#[cfg(feature = "sync")]
const TOKEN_WAKEUP: mio::Token = mio::Token(1 << 31);

#[allow(dead_code)]
impl LegacyDriver {
    const DEFAULT_ENTRIES: u32 = 1024;

    pub(crate) fn new() -> io::Result<Self> {
        Self::new_with_entries(Self::DEFAULT_ENTRIES)
    }

    pub(crate) fn new_with_entries(entries: u32) -> io::Result<LegacyDriver> {
        #[cfg(unix)]
        let mioPoll = mio::Poll::new()?;

        #[cfg(all(unix, feature = "sync"))]
        let mioWakerWrapper = Arc::new(MioWakerWrapper::new(mio::Waker::new(mioPoll.registry(), TOKEN_WAKEUP)?));

        #[cfg(feature = "sync")]
        let (wakerSender, wakerReceiver) = flume::unbounded::<std::task::Waker>();

        #[cfg(feature = "sync")]
        let threadId = crate::builder::BUILD_THREAD_ID.with(|id| *id);

        let legacyInner = LegacyDriverInner {
            io_dispatch: Slab::new(),
            #[cfg(unix)]
            mioEvents: mio::Events::with_capacity(entries as usize),
            #[cfg(unix)]
            mioPoll,
            #[cfg(feature = "sync")]
            mioWakerWrapper,
            #[cfg(feature = "sync")]
            wakerReceiver,
        };

        let legacyDriver = LegacyDriver {
            legacyDriverInner: Rc::new(UnsafeCell::new(legacyInner)),
            threadId,
        };

        // Register unpark handle
        #[cfg(feature = "sync")]
        {
            let unpark = legacyDriver.getUnpark();
            super::thread::register_unpark_handle(threadId, unpark.into());
            super::thread::register_waker_sender(threadId, wakerSender);
        }

        Ok(legacyDriver)
    }

    fn inner_park(&self, mut timeout: Option<Duration>) -> io::Result<()> {
        let legacyDriverInner = unsafe { &mut *self.legacyDriverInner.get() };

        #[allow(unused_mut)]
        let mut needWait = true;

        #[cfg(feature = "sync")]
        {
            // process foreign wakers
            while let Ok(waker) = legacyDriverInner.wakerReceiver.try_recv() {
                waker.wake();
                needWait = false;
            }

            // set status as not awake if we are going to sleep
            if needWait {
                legacyDriverInner.mioWakerWrapper.awake.store(false, Ordering::Release);
            }

            // process foreign wakers left
            while let Ok(waker) = legacyDriverInner.wakerReceiver.try_recv() {
                waker.wake();
                needWait = false;
            }
        }

        if !needWait {
            timeout = Some(Duration::ZERO);
        }

        // here we borrow 2 mut self, but its safe.
        let mioEvents = unsafe { &mut (*self.legacyDriverInner.get()).mioEvents };

        match legacyDriverInner.mioPoll.poll(mioEvents, timeout) {
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }

        for mioEvent in mioEvents.iter() {
            let token = mioEvent.token();

            #[cfg(feature = "sync")]
            if token != TOKEN_WAKEUP {
                legacyDriverInner.dispatch(token, Ready::fromMioEvent(mioEvent));
            }

            #[cfg(not(feature = "sync"))]
            inner.dispatch(token, Ready::fromMioEvent(mioEvent));
        }

        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn register(legacyInner: &Rc<UnsafeCell<LegacyDriverInner>>,
                           source: &mut impl mio::event::Source,
                           interest: mio::Interest) -> io::Result<usize> {
        let legacyInner = unsafe { &mut *legacyInner.get() };
        let indexInSlab = legacyInner.io_dispatch.insert(ScheduledIo::new());

        let registry = legacyInner.mioPoll.registry();

        match registry.register(source, mio::Token(indexInSlab), interest) {
            Ok(_) => Ok(indexInSlab),
            Err(e) => {
                legacyInner.io_dispatch.remove(indexInSlab);
                Err(e)
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn deregister(legacyDriverInner: &Rc<UnsafeCell<LegacyDriverInner>>,
                             indexInSlab: usize,
                             source: &mut impl mio::event::Source) -> io::Result<()> {
        let legacyDriverInner = unsafe { &mut *legacyDriverInner.get() };

        // try to deregister fd first, on success we will remove it from slab.
        match legacyDriverInner.mioPoll.registry().deregister(source) {
            Ok(_) => {
                legacyDriverInner.io_dispatch.remove(indexInSlab);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl LegacyDriverInner {
    fn dispatch(&mut self, mioToken: mio::Token, ready: Ready) {
        let mut scheduledIo =
            match self.io_dispatch.get(mioToken.0) {
                Some(scheduledIo) => scheduledIo,
                None => return
            };

        let scheduledIo = scheduledIo.as_mut();
        scheduledIo.setReady(|curr| curr | ready);
        scheduledIo.wake(ready);
    }

    pub(crate) fn pollOpAble<T: OpAble>(legacyInner: &Rc<UnsafeCell<LegacyDriverInner>>,
                                        opAble: &mut T,
                                        context: &mut Context<'_>) -> Poll<CompletionMeta> {
        let legacyDriverInner = unsafe { &mut *legacyInner.get() };

        let (directionInterest, index) = match opAble.legacy_interest() {
            Some(x) => x,
            None => {  // the action does not rely on fd readiness. do syscall right now.
                return Poll::Ready(CompletionMeta {
                    result: opAble.legacy_call(),
                    flags: 0,
                });
            }
        };

        // wait io ready and do syscall
        // 实际得到的io
        let mut scheduledIo = legacyDriverInner.io_dispatch.get(index).expect("scheduled_io lost");
        let scheduledIo = scheduledIo.as_mut();

        // 会设置waker
        let ready = ready!(scheduledIo.isReady(context, directionInterest));

        // check if canceled
        if ready.is_canceled() {
            // clear CANCELED part only
            scheduledIo.clearReady(ready & Ready::CANCELED);

            return Poll::Ready(CompletionMeta {
                result: Err(io::Error::from_raw_os_error(125)),
                flags: 0,
            });
        }

        match opAble.legacy_call() {
            Ok(result) => Poll::Ready(CompletionMeta {
                result: Ok(result),
                flags: 0,
            }),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                scheduledIo.clearReady(directionInterest.mask());
                scheduledIo.setWaker(context, directionInterest);
                Poll::Pending
            }
            Err(e) => Poll::Ready(CompletionMeta {
                result: Err(e),
                flags: 0,
            }),
        }
    }

    pub(crate) fn cancel_op(
        this: &Rc<UnsafeCell<LegacyDriverInner>>,
        index: usize,
        direction: ready::Direction,
    ) {
        let inner = unsafe { &mut *this.get() };
        let ready = match direction {
            ready::Direction::Read => Ready::READ_CANCELED,
            ready::Direction::Write => Ready::WRITE_CANCELED,
        };
        inner.dispatch(mio::Token(index), ready);
    }

    pub(crate) fn submitOpAble<T: OpAble>(legacyDriverInner: &Rc<UnsafeCell<LegacyDriverInner>>, opAble: T) -> io::Result<Op<T>> {
        Ok(Op {
            driverInner: DriverInner::Legacy(legacyDriverInner.clone()),
            // useless for legacy
            index: 0,
            opAble: Some(opAble),
        })
    }

    #[cfg(feature = "sync")]
    pub(crate) fn getLegacyUnpark(this: &Rc<UnsafeCell<LegacyDriverInner>>) -> LegacyUnpark {
        let inner = unsafe { &*this.get() };
        LegacyUnpark(Arc::downgrade(&inner.mioWakerWrapper))
    }
}

impl Driver for LegacyDriver {
    fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        let inner = DriverInner::Legacy(self.legacyDriverInner.clone());
        CURRENT_INNER.set(&inner, f)
    }

    fn submit(&self) -> io::Result<()> {
        self.inner_park(Some(Duration::ZERO))
    }

    fn park(&self) -> io::Result<()> {
        self.inner_park(None)
    }

    fn park_timeout(&self, duration: Duration) -> io::Result<()> {
        self.inner_park(Some(duration))
    }

    #[cfg(feature = "sync")]
    type Unpark = LegacyUnpark;

    #[cfg(feature = "sync")]
    fn getUnpark(&self) -> Self::Unpark {
        LegacyDriverInner::getLegacyUnpark(&self.legacyDriverInner)
    }
}

impl Drop for LegacyDriver {
    fn drop(&mut self) {
        // Deregister thread id
        #[cfg(feature = "sync")]
        {
            use crate::driver::thread::{unregister_unpark_handle, unregister_waker_sender};
            unregister_unpark_handle(self.threadId);
            unregister_waker_sender(self.threadId);
        }
    }
}
