use std::task::{Context, Poll, Waker};

use super::ready::{Direction, Ready};

pub(crate) struct ScheduledIo {
    ready: Ready,

    /// Waker used for AsyncRead.
    readWaker: Option<Waker>,

    /// Waker used for AsyncWrite.
    writeWaker: Option<Waker>,
}

impl Default for ScheduledIo {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduledIo {
    pub(crate) const fn new() -> Self {
        Self {
            ready: Ready::EMPTY,
            readWaker: None,
            writeWaker: None,
        }
    }

    #[allow(unused)]
    #[inline]
    pub(crate) fn set_writable(&mut self) {
        self.ready |= Ready::WRITABLE;
    }

    #[inline]
    pub(crate) fn setReady(&mut self, f: impl Fn(Ready) -> Ready) {
        self.ready = f(self.ready);
    }

    #[inline]
    pub(crate) fn wake(&mut self, ready: Ready) {
        if ready.is_readable() {
            if let Some(waker) = self.readWaker.take() {
                waker.wake();
            }
        }

        if ready.is_writable() {
            if let Some(waker) = self.writeWaker.take() {
                waker.wake();
            }
        }
    }

    #[inline]
    pub(crate) fn clearReady(&mut self, ready: Ready) {
        self.ready = self.ready - ready;
    }

    /// 要是当前的io还不是想要的话,设置相应的waker,等到有了想要的时候调用waker.wake()
    pub(crate) fn isReady(&mut self,
                          cx: &mut Context<'_>,
                          direction: Direction) -> Poll<Ready> {
        let ready = direction.mask() & self.ready;
        if !ready.is_empty() {
            return Poll::Ready(ready);
        }

        self.set_waker(cx, direction);

        Poll::Pending
    }

    pub(crate) fn set_waker(&mut self, cx: &mut Context<'_>, direction: Direction) {
        let slot = match direction {
            Direction::Read => &mut self.readWaker,
            Direction::Write => &mut self.writeWaker,
        };

        match slot {
            Some(existing) => {
                if !existing.will_wake(cx.waker()) {
                    existing.clone_from(cx.waker());
                }
            }
            None => {
                *slot = Some(cx.waker().clone());
            }
        }
    }
}
