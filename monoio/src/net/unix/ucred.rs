// Forked from tokio.
use libc::{gid_t, pid_t, uid_t};

/// Credentials of a process
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct UCred {
    /// PID (process ID) of the process
    pid: Option<pid_t>,
    /// UID (user ID) of the process
    uid: uid_t,
    /// GID (group ID) of the process
    gid: gid_t,
}

impl UCred {
    /// Gets UID (user ID) of the process.
    pub fn uid(&self) -> uid_t {
        self.uid
    }

    /// Gets GID (group ID) of the process.
    pub fn gid(&self) -> gid_t {
        self.gid
    }

    /// Gets PID (process ID) of the process.
    pub fn pid(&self) -> Option<pid_t> {
        self.pid
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "openbsd"))]
pub(crate) use self::impl_linux::get_peer_cred;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use self::impl_macos::get_peer_cred;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod impl_macos {
    use std::{
        io,
        mem::{size_of, MaybeUninit},
        os::unix::io::AsRawFd,
    };

    use libc::{c_void, getpeereid, getsockopt, pid_t, LOCAL_PEEREPID, SOL_LOCAL};

    use crate::net::unix::UnixStream;

    pub(crate) fn get_peer_cred(sock: &UnixStream) -> io::Result<super::UCred> {
        unsafe {
            let raw_fd = sock.as_raw_fd();

            let mut uid = MaybeUninit::uninit();
            let mut gid = MaybeUninit::uninit();
            let mut pid: MaybeUninit<pid_t> = MaybeUninit::uninit();
            let mut pid_size: MaybeUninit<u32> = MaybeUninit::new(size_of::<pid_t>() as u32);

            if getsockopt(
                raw_fd,
                SOL_LOCAL,
                LOCAL_PEEREPID,
                pid.as_mut_ptr() as *mut c_void,
                pid_size.as_mut_ptr(),
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }

            assert!(pid_size.assume_init() == (size_of::<pid_t>() as u32));

            let ret = getpeereid(raw_fd, uid.as_mut_ptr(), gid.as_mut_ptr());

            if ret == 0 {
                Ok(super::UCred {
                    uid: uid.assume_init(),
                    gid: gid.assume_init(),
                    pid: Some(pid.assume_init()),
                })
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "openbsd"))]
pub(crate) mod impl_linux {
    use std::{io, mem};

    #[cfg(target_os = "openbsd")]
    use libc::sockpeercred as ucred;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use libc::ucred;
    use libc::{c_void, getsockopt, socklen_t, SOL_SOCKET, SO_PEERCRED};

    use crate::net::unix::UnixStream;

    pub(crate) fn get_peer_cred(sock: &UnixStream) -> io::Result<super::UCred> {
        use std::os::unix::io::AsRawFd;

        unsafe {
            let raw_fd = sock.as_raw_fd();

            let mut ucred = ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };

            let ucred_size = mem::size_of::<ucred>();

            // These paranoid checks should be optimized-out
            assert!(mem::size_of::<u32>() <= mem::size_of::<usize>());
            assert!(ucred_size <= u32::MAX as usize);

            let mut ucred_size = ucred_size as socklen_t;

            let ret = getsockopt(
                raw_fd,
                SOL_SOCKET,
                SO_PEERCRED,
                &mut ucred as *mut ucred as *mut c_void,
                &mut ucred_size,
            );
            if ret == 0 && ucred_size as usize == mem::size_of::<ucred>() {
                Ok(super::UCred {
                    uid: ucred.uid,
                    gid: ucred.gid,
                    pid: Some(ucred.pid),
                })
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}
