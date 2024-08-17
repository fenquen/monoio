use super::{IoBuf, IoBufMut, IoVecBuf, IoVecBufMut};

pub(crate) struct IoVecMeta {
    #[cfg(unix)]
    data: Vec<libc::iovec>,
    offset: usize,
    len: usize,
}

/// Read IoVecBuf meta data into a Vec.
pub(crate) fn read_vec_meta<T: IoVecBuf>(buf: &T) -> IoVecMeta {
    #[cfg(unix)]
    {
        let ptr = buf.read_iovec_ptr();
        let iovec_len = buf.read_iovec_len();

        let mut data = Vec::with_capacity(iovec_len);
        let mut len = 0;
        for i in 0..iovec_len {
            let iovec = unsafe { *ptr.add(i) };
            data.push(iovec);
            len += iovec.iov_len;
        }
        IoVecMeta {
            data,
            offset: 0,
            len,
        }
    }
}

/// Read IoVecBufMut meta data into a Vec.
pub(crate) fn write_vec_meta<T: IoVecBufMut>(buf: &mut T) -> IoVecMeta {
    #[cfg(unix)]
    {
        let ptr = buf.write_iovec_ptr();
        let iovec_len = buf.write_iovec_len();

        let mut data = Vec::with_capacity(iovec_len);
        let mut len = 0;
        for i in 0..iovec_len {
            let iovec = unsafe { *ptr.add(i) };
            data.push(iovec);
            len += iovec.iov_len;
        }
        IoVecMeta {
            data,
            offset: 0,
            len,
        }
    }
}

impl IoVecMeta {
    #[allow(unused_mut)]
    pub(crate) fn consume(&mut self, mut amt: usize) {
        #[cfg(unix)]
        {
            if amt == 0 {
                return;
            }
            let mut offset = self.offset;
            while let Some(iovec) = self.data.get_mut(offset) {
                match iovec.iov_len.cmp(&amt) {
                    std::cmp::Ordering::Less => {
                        amt -= iovec.iov_len;
                        offset += 1;
                        continue;
                    }
                    std::cmp::Ordering::Equal => {
                        offset += 1;
                        self.offset = offset;
                        return;
                    }
                    std::cmp::Ordering::Greater => {
                        let _ = unsafe { iovec.iov_base.add(amt) };
                        iovec.iov_len -= amt;
                        self.offset = offset;
                        return;
                    }
                }
            }
            panic!("try to consume more than owned")
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

unsafe impl IoVecBuf for IoVecMeta {
    #[cfg(unix)]
    fn read_iovec_ptr(&self) -> *const libc::iovec {
        unsafe { self.data.as_ptr().add(self.offset) }
    }

    #[cfg(unix)]
    fn read_iovec_len(&self) -> usize {
        self.data.len()
    }
}

unsafe impl IoVecBufMut for IoVecMeta {
    #[cfg(unix)]
    fn write_iovec_ptr(&mut self) -> *mut libc::iovec {
        unsafe { self.data.as_mut_ptr().add(self.offset) }
    }

    #[cfg(unix)]
    fn write_iovec_len(&mut self) -> usize {
        self.data.len()
    }

    unsafe fn set_init(&mut self, pos: usize) {
        self.consume(pos)
    }
}

impl<'t, T: IoBuf> From<&'t T> for IoVecMeta {
    fn from(buf: &'t T) -> Self {
        let ptr = buf.read_ptr() as *const _ as *mut _;
        let len = buf.bytes_init() as _;
        #[cfg(unix)]
        let item = libc::iovec {
            iov_base: ptr,
            iov_len: len,
        };

        Self {
            data: vec![item],
            offset: 0,
            len: 1,
        }
    }
}

impl<'t, T: IoBufMut> From<&'t mut T> for IoVecMeta {
    fn from(buf: &'t mut T) -> Self {
        let ptr = buf.write_ptr() as *mut _;
        let len = buf.bytes_total() as _;
        #[cfg(unix)]
        let item = libc::iovec {
            iov_base: ptr,
            iov_len: len,
        };

        Self {
            data: vec![item],
            offset: 0,
            len: 1,
        }
    }
}