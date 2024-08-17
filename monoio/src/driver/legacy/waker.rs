use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Weak;
use crate::driver::unpark::Unpark;

pub(crate) struct MioWakerWrapper {
    #[cfg(unix)]
    mioWaker: mio::Waker,
    pub(crate) awake: AtomicBool,
}

impl MioWakerWrapper {
    #[cfg(unix)]
    pub(crate) fn new(waker: mio::Waker) -> Self {
        Self {
            mioWaker: waker,
            awake: AtomicBool::new(true),
        }
    }

    pub(crate) fn wake(&self) -> std::io::Result<()> {
        // already awake
        if self.awake.load(Ordering::Acquire) {
            return Ok(());
        }

        self.mioWaker.wake()
    }
}

#[derive(Clone)]
pub struct LegacyUnpark(pub(crate) Weak<MioWakerWrapper>);

impl Unpark for LegacyUnpark {
    fn unpark(&self) -> std::io::Result<()> {
        if let Some(mioWakerWrapper) = self.0.upgrade() {
            mioWakerWrapper.wake()
        } else {
            Ok(())
        }
    }
}
