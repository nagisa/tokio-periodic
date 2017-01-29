//! Asynchronous, periodic, mostly-0-cost cross-platform timers

#[macro_use]
extern crate tokio_core;
extern crate mio;
extern crate futures;

use std::time::Duration;
use tokio_core::reactor;
use std::io::{Result, Read};

/// Asynchronous, periodic timer
///
/// This timer, when `reset` for the first time will become ready to `poll` every specified
/// interval until it is destroyed.
pub struct PeriodicTimer {
    poll: reactor::PollEvented<imp::Timer>
}

impl PeriodicTimer {
    /// Create a new `PeriodicTimer` associated with specified event loop handle
    pub fn new(handle: &reactor::Handle) -> Result<PeriodicTimer> {
        Ok(PeriodicTimer {
            poll: try!(reactor::PollEvented::new(try!(imp::Timer::new()), handle))
        })
    }

    /// Reset the timer to specified `interval`
    ///
    /// The previously active timer, if any, is cancelled before the new one is set. Once this
    /// method returns, this timer will become ready to `poll` every `interval` duration.
    ///
    /// Due to some platforms not supporting nanosecond precision the interval duration is rounded
    /// up to the nearest precision supported by the platform.
    ///
    /// If the `interval` is zero-duration, the timer is deactivated.
    ///
    /// # Errors
    ///
    /// In addition to the errors which may occur when interacting with system APIs, an error
    /// condition may also occur when `interval` duration exceeds the maximum duration that can be
    /// specified to the system.
    pub fn reset(&self, interval: Duration) -> Result<()> {
        imp::Timer::reset(self.poll.get_ref(), interval)
    }
}

impl futures::stream::Stream for PeriodicTimer {
    type Item = u64;
    type Error = std::io::Error;

    /// Poll the timer
    ///
    /// If the timer has fired at least once since previous call to `poll` or `reset`, this
    /// resolves to a number of times the timer has fired.
    fn poll(&mut self) -> futures::Poll<Option<Self::Item>, Self::Error> {
        let mut buf = [0xff; 8];
        let bs = try_nb!(self.poll.read(&mut buf));
        debug_assert_eq!(bs, 8);
        Ok(futures::Async::Ready(Some(unsafe { *(buf.as_ptr() as *const u64) })))
    }
}

#[cfg(target_os="linux")]
mod imp {
    extern crate libc;
    use std::os::unix::io::RawFd;
    use std::os::raw::c_int;
    use std::io::Result;
    use std::time::Duration;
    use mio::{Poll, Token, Ready, PollOpt, Evented};
    use mio::unix::EventedFd;

    pub struct Timer { fd: RawFd }

    const TFD_CLOEXEC: c_int = libc::O_CLOEXEC;
    const TFD_NONBLOCK: c_int = libc::O_NONBLOCK;

    impl Timer {
        pub fn new() -> Result<Timer> {
            unsafe {
                let ret = timerfd_create(libc::CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
                if ret == -1 { return Err(::std::io::Error::last_os_error()); }
                Ok(Timer {
                    fd: ret
                })
            }
        }

        pub fn reset(&self, interval: Duration) -> Result<()> {
            // This seriously needs a nicer way to convert.
            let tspec = libc::timespec {
                tv_sec: interval.as_secs() as _,
                tv_nsec: interval.subsec_nanos() as _,
            };
            let itspec = itimerspec {
                interval: tspec,
                initial: tspec,
            };
            unsafe {
                let ret = timerfd_settime(self.fd, 0, &itspec, ::std::ptr::null_mut());
                if ret == -1 { return Err(::std::io::Error::last_os_error()); }
                Ok(())
            }
        }
    }

    impl Evented for Timer {
        fn register(&self, poll: &Poll, token: Token, _: Ready, opts: PollOpt) -> Result<()> {
            EventedFd(&self.fd).register(poll, token, Ready::readable() | Ready::error(), opts)
        }

        fn reregister(&self, poll: &Poll, token: Token, _: Ready, opts: PollOpt) -> Result<()> {
            EventedFd(&self.fd).reregister(poll, token, Ready::readable() | Ready::error(), opts)
        }

        fn deregister(&self, poll: &Poll) -> Result<()> {
            EventedFd(&self.fd).deregister(poll)
        }
    }

    impl ::std::io::Read for Timer {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let ret = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if ret == -1 { return Err(::std::io::Error::last_os_error()); }
            debug_assert_eq!(ret, 8);
            Ok(ret as usize)
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }

    #[repr(C)]
    struct itimerspec {
        interval: libc::timespec,
        initial: libc::timespec,
    }
    extern {
        fn timerfd_create(clockid: c_int, flags: c_int) -> RawFd;
        fn timerfd_settime(fd: RawFd, flags: c_int,
                           new_ts: *const itimerspec,
                           old_ts: *mut itimerspec) -> c_int;
    }
}

#[cfg(any(target_os = "bitrig", target_os = "dragonfly", target_os = "freebsd", target_os = "ios",
          target_os = "macos", target_os = "netbsd", target_os = "openbsd"))]
mod imp {
    extern crate libc;
    use std::os::unix::io::RawFd;
    use std::io::Result;
    use std::time::Duration;
    use self::libc::{intptr_t, kevent, kqueue};
    use mio::{Poll, Token, Ready, PollOpt, Evented};
    use mio::unix::EventedFd;

    pub struct Timer { fd: RawFd }

    impl Timer {
        pub fn new() -> Result<Timer> {
            unsafe {
                let ret = kqueue();
                if ret == -1 { return Err(::std::io::Error::last_os_error()); }
                if libc::ioctl(ret, libc::FIOCLEX) == -1 {
                    libc::fcntl(ret, libc::F_SETFD, libc::FD_CLOEXEC);
                }
                Ok(Timer { fd: ret })
            }
        }

        pub fn reset(&self, interval: Duration) -> Result<()> {
            let (ty, dur) = try!(Timer::duration_to_units(interval));
            let en_flag = if dur == 0 { libc::EV_DISABLE } else { libc::EV_ENABLE };
            let kevt = kevent {
                ident: 0,
                filter: libc::EVFILT_TIMER,
                flags: libc::EV_ADD | en_flag,
                fflags: ty,
                data: dur,
                udata: ::std::ptr::null_mut()
            };
            unsafe {
                let ret = kevent(self.fd, &kevt, 1,
                                       ::std::ptr::null_mut(), 0,
                                       ::std::ptr::null());
                if ret == -1 { return Err(::std::io::Error::last_os_error()); }
                Ok(())
            }
        }

        // intptr_t for time lolz?
        fn duration_to_units(interval: Duration) -> Result<(u32, intptr_t)> {
            let secs = interval.as_secs();
            let nanos = interval.subsec_nanos() as u64;
            let max = intptr_t::max_value() as u64;
            if nanos > max || secs > max {
                return Err(::std::io::Error::from_raw_os_error(libc::EINVAL));
            }
            let (ty, subsec) = if nanos % 1_000_000_000 == 0 {
                (libc::NOTE_SECONDS, 0)
            } else if nanos % 1_000_000 == 0 {
                (NOTE_MSECONDS, nanos / 1_000_0000)
            } else if nanos % 1_000 == 0 {
                (libc::NOTE_USECONDS, nanos / 1_000)
            } else {
                (libc::NOTE_NSECONDS, nanos)
            };
            let combined = match ty {
                libc::NOTE_SECONDS => Some(secs as intptr_t),
                NOTE_MSECONDS =>
                    (secs as intptr_t).checked_mul(1_000)
                                      .and_then(|v| v.checked_add(subsec as intptr_t)),
                libc::NOTE_USECONDS =>
                    (secs as intptr_t).checked_mul(1_000_000)
                                      .and_then(|v| v.checked_add(subsec as intptr_t)),
                libc::NOTE_NSECONDS =>
                    (secs as intptr_t).checked_mul(1_000_000_000)
                                      .and_then(|v| v.checked_add(subsec as intptr_t)),
                _ => panic!("impossible case reached")
            };
            Ok((ty, if let Some(ret) = combined {
                ret
            } else {
                return Err(::std::io::Error::from_raw_os_error(libc::EINVAL));
            }))
        }
    }

    // On OS X MSECONDS is the default if nothing else is specified.
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    const NOTE_MSECONDS: u32 = 0;
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    const NOTE_MSECONDS: u32 = libc::NOTE_MSECONDS;

    impl Evented for Timer {
        fn register(&self, poll: &Poll, token: Token, _: Ready, opts: PollOpt) -> Result<()> {
            EventedFd(&self.fd).register(poll, token, Ready::readable() | Ready::error(), opts)
        }

        fn reregister(&self, poll: &Poll, token: Token, _: Ready, opts: PollOpt) -> Result<()> {
            EventedFd(&self.fd).reregister(poll, token, Ready::readable() | Ready::error(), opts)
        }

        fn deregister(&self, poll: &Poll) -> Result<()> {
            EventedFd(&self.fd).deregister(poll)
        }
    }

    impl ::std::io::Read for Timer {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            unsafe {
                // Need at least 8 bytes of buffer
                if buf.len() < 8 { return Err(::std::io::Error::from_raw_os_error(libc::EINVAL)); }
                let mut kevt: kevent = ::std::mem::uninitialized();
                let tspec = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                let evts = kevent(self.fd, ::std::ptr::null(), 0,
                                        &mut kevt, 1, &tspec);
                if evts == -1 {
                    return Err(::std::io::Error::last_os_error());
                } else if evts == 1 {
                    if kevt.filter != libc::EVFILT_TIMER { return Ok(0) } // wtf?
                    let ptr = buf.as_mut_ptr() as *mut u64;
                    *ptr = kevt.data as u64;
                    return Ok(8)
                } else {
                    return Err(::mio::would_block())
                }
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

#[cfg(target_os="windows")]
mod imp {
    extern crate winapi;
    extern crate kernel32;
    use std::sync::atomic;
    use std::ptr;
    use std::io::Result;
    use std::time::Duration;
    use mio::{Poll, Token, Ready, PollOpt, Evented, Registration, SetReadiness};

    pub struct Timer {
        inner: *mut Inner
    }

    pub struct Inner {
        active: atomic::AtomicPtr<winapi::c_void>,
        times_fired: atomic::AtomicUsize,
        registration: ::std::sync::Mutex<Option<(Registration, SetReadiness)>>,

    }

    impl Timer {
        pub fn new() -> Result<Timer> {
            Ok(Timer {
                inner: Box::into_raw(Box::new(Inner {
                    active: atomic::AtomicPtr::new(ptr::null_mut()),
                    times_fired: atomic::AtomicUsize::new(0),
                    registration: ::std::sync::Mutex::new(None)
                }))
            })
        }

        pub fn reset(&self, interval: Duration) -> Result<()> {
            unsafe extern "system" fn callback(data: winapi::PVOID,
                                               _: winapi::BOOLEAN) {
                let this: &Inner = &*(data as *mut _);
                let _ = this.times_fired.fetch_add(1, atomic::Ordering::SeqCst);
                match this.registration.lock().unwrap().as_ref() {
                    Some(&(_, ref s)) => {
                        // Can’t error from here, sadly.
                        let _ = s.set_readiness(Ready::readable());
                    },
                    None => {}
                }
            }
            unsafe {
                let mut out = (*self.inner).active.swap(ptr::null_mut(),
                                                        atomic::Ordering::SeqCst);
                if !out.is_null() {
                    let ret = kernel32::DeleteTimerQueueTimer(ptr::null_mut(), out,
                                                              winapi::INVALID_HANDLE_VALUE);
                    if ret == 0 { return Err(::std::io::Error::last_os_error()); }
                }
                (*self.inner).times_fired.store(0, atomic::Ordering::SeqCst);
                let time_in_ms = try!(Timer::interval_to_millis(interval));
                if time_in_ms == 0 { return Ok(()); }
                let ret = kernel32::CreateTimerQueueTimer(&mut out, ptr::null_mut(),
                                                          Some(callback),
                                                          // not mutated, so its fine
                                                          self.inner as *mut _,
                                                          time_in_ms, time_in_ms,
                                                          0);
                if ret == 0 { return Err(::std::io::Error::last_os_error()); }
                // FIXME: probably should just loop (or deregister)
                (*self.inner).active.compare_exchange(ptr::null_mut(), out,
                                                      atomic::Ordering::SeqCst,
                                                      atomic::Ordering::SeqCst)
                .expect("invariant broken");
                Ok(())
            }
        }

        pub fn interval_to_millis(interval: Duration) -> Result<winapi::DWORD> {
            let max = winapi::DWORD::max_value();
            // Round up
            let subsec_ns = interval.subsec_nanos() as u64 + 999_999;
            let ms = interval.as_secs().checked_mul(1_000)
                .and_then(|v| v.checked_add(subsec_ns / 1_000_000));
            if let Some(ms) = ms {
                if ms <= max as _ {
                    return Ok(ms as _);
                }
            }
            Err(::std::io::Error::from_raw_os_error(winapi::ERROR_INVALID_PARAMETER as i32))
        }
    }

    impl Evented for Timer {
        fn register(&self, poll: &Poll, token: Token, rdy: Ready, opts: PollOpt) -> Result<()> {
            unsafe {
                let val = Some(Registration::new(poll, token, rdy, opts));
                *(*self.inner).registration.lock().unwrap() = val;
                Ok(())
            }
        }

        fn reregister(&self, poll: &Poll, token: Token, rdy: Ready, opts: PollOpt) -> Result<()> {
            unsafe {
                (*self.inner).registration.lock().unwrap().as_mut().unwrap().0
                .update(poll, token, rdy, opts)
            }
        }

        fn deregister(&self, poll: &Poll) -> Result<()> {
            unsafe {
                (*self.inner).registration.lock().unwrap().take().unwrap().0.deregister(poll)
            }
        }
    }

    impl ::std::io::Read for Timer {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            unsafe {
                if buf.len() < 8 { return Err(
                    ::std::io::Error::from_raw_os_error(winapi::ERROR_INSUFFICIENT_BUFFER as i32)
                ); }
                let ptr = buf.as_mut_ptr() as *mut u64;
                *ptr = (*self.inner).times_fired.swap(0, atomic::Ordering::SeqCst) as u64;
                if *ptr == 0 {
                    return Err(::mio::would_block());
                }
                Ok(8)
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe {
                let out = (*self.inner).active.swap(ptr::null_mut(), atomic::Ordering::SeqCst);
                if !out.is_null() {
                    let ret = kernel32::DeleteTimerQueueTimer(ptr::null_mut(), out,
                                                              winapi::INVALID_HANDLE_VALUE);
                    if ret == 0 {
                        panic!("DeleteTimerQueueTimer failed with {:?}",
                               ::std::io::Error::last_os_error());
                    }
                }
                Box::from_raw(self.inner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time;
    use tokio_core::reactor;
    use futures::stream::Stream;
    use futures::future::{self, Future};

    #[cfg(windows)]
    fn allowed_delta() -> time::Duration {
        // Windows has horrendous scheduling wherein a thread could take 16ms to wake up or
        // something. Give it 20ms to figure out what’s happening :)
        time::Duration::new(0, 20_000_000)
    }

    #[cfg(not(windows))]
    fn allowed_delta() -> time::Duration {
        // Every other OS is swifter and can handle 5ms and probably even smaller deltas
        // comfortably.
        time::Duration::new(0, 5_000_000)
    }

    #[test]
    fn works() {
        let mut core = reactor::Core::new().unwrap();
        let handle = core.handle();
        let mut timer = super::PeriodicTimer::new(&handle)
            .expect("periodic timer can be created");
        let interval = time::Duration::new(0, 32_000_000);
        timer.reset(interval)
            .expect("reset works");
        for _ in 0..3 {
            let start = time::Instant::now();
            let res = core.run(timer.into_future());
            timer = match res {
                Ok((Some(1), timer)) => timer,
                Ok((x, _)) => panic!("expected Ok((Some(1), _)), got Ok(({:?}, _))", x),
                Err((x, _)) => panic!("expected Ok((Some(1), _)), got Err(({:?}, _))", x),
            };
            let duration = time::Instant::now().duration_since(start);
            let absdiff = if duration < interval {
                interval - duration
            } else {
                duration - interval
            };
            assert!(absdiff < allowed_delta(), "absdiff is {:?}", absdiff);
        }
    }

    #[test]
    fn reset_works() {
        let mut core = reactor::Core::new().unwrap();
        let handle = core.handle();
        let mut timer = super::PeriodicTimer::new(&handle)
            .expect("periodic timer can be created");
        timer.reset(time::Duration::new(0, 5_000_000))
            .expect("reset works");
        // run one iteration, so in case reset worked incrrectly and set up two timers,
        // timer would then fire in a pattern of `a-b-[32ms of a]-b-[32ms of a]-b-...` and test
        // would fail
        timer = core.run(timer.into_future()).map_err(|(a, _)| a).unwrap().1;
        let interval = time::Duration::new(0, 32_000_000);
        timer.reset(interval)
            .expect("reset works");
        for _ in 0..3 {
            let start = time::Instant::now();
            let res = core.run(timer.into_future());
            timer = match res {
                Ok((Some(1), timer)) => timer,
                Ok((x, _)) => panic!("expected Ok((Some(1), _)), got Ok(({:?}, _))", x),
                Err((x, _)) => panic!("expected Ok((Some(1), _)), got Err(({:?}, _))", x),
            };
            let duration = time::Instant::now().duration_since(start);
            let absdiff = if duration < interval {
                interval - duration
            } else {
                duration - interval
            };
            assert!(absdiff < allowed_delta(), "absdiff is {:?}", absdiff);
        }
    }

    #[test]
    fn drop_unset_works() {
        let core = reactor::Core::new().unwrap();
        let handle = core.handle();
        super::PeriodicTimer::new(&handle).expect("periodic timer can be created");
    }

    #[test]
    fn reset_zero_works() {
        let mut core = reactor::Core::new().unwrap();
        let handle = core.handle();
        let timer = super::PeriodicTimer::new(&handle).expect("periodic timer can be created");
        timer.reset(time::Duration::new(0, 1_000_000)).expect("reset works");
        timer.reset(time::Duration::new(0, 0)).expect("reset zero works");
        ::std::thread::sleep(time::Duration::new(0, 100_000_000));
        let future = timer.into_future().map(|(a, _)| a).map_err(|(a, _)| a)
                          .select(future::ok(Some(9876)));
        let resets = core.run(future).map_err(|(a, _)| a).unwrap().0;
        assert_eq!(resets, Some(9876)); // timer never fired, so the other future got selected.
    }
}
