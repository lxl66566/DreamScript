//! The CLI host's shutdown door (the 2026-11-24 signal ruling): the
//! process's SIGINT/SIGTERM translate into one shutdown event handed to
//! the run, and the language never sees a Unix signal. The signals are
//! blocked on the main thread before anything else runs — mask
//! inheritance carries the block to every thread the run later spawns
//! (the fork-join workers, the resolver helper), so the default
//! disposition dies nowhere inside the process.
//!
//! The Linux shape is a signalfd over the OS's own C symbols (the
//! raw-socket round's precedent, no signal crate): the fd registers as
//! the reactor's own interest, its wake IS the shutdown event, and the
//! drive's cut answers [`ds_vm::Trap::Shutdown`] — parked fibers observe
//! the cancel sentinel at their resume-shaped points with their deinit
//! hooks fired, the engine deciding nothing. Every other host keeps the
//! blocking twins, so its shape is a minimal helper flipping the stop
//! flag the blocking accept poll observes between rounds — a sigwait
//! thread on the other unixes, a console-control handler on Windows.
//! The exit code is the host's own per the die split: Unix answers
//! 128+signum, Windows the failed-run row (no signum exists for a
//! console event).

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        ffi::c_int,
        fs::File,
        io::Read,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        process::ExitCode,
    };

    use anyhow::{Result, anyhow};

    use super::unix_imp::{SIGINT, SIGTERM, SigSet};

    unsafe extern "C" {
        // The OS's own symbol (no signal crate), the platform header's
        // own signature: the mask is the userspace sigset the wrapper
        // converts for the kernel, and the third word is the flags
        // register (current glibc's flags, musl's ignored sizemask) —
        // zero answering the blocking fd the reactor's poll wants
        fn signalfd(fd: c_int, mask: *const SigSet, flags: c_int) -> c_int;
        // the non-blocking flip over the fd the raw-socket round's own
        // fcntl precedent — the post-run signum read must answer
        // EAGAIN when no signal stands, never park the exiting host
        fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int;
    }

    /// F_SETFL, the fcntl operation that sets the descriptor's flags.
    const F_SETFL: c_int = 4;
    /// O_NONBLOCK, the flag that answers EAGAIN instead of parking.
    const O_NONBLOCK: c_int = 0x800;

    /// The host's shutdown interest: one signalfd whose readable wake
    /// the drive reads as the shutdown event.
    pub struct ShutdownWatch {
        fd: OwnedFd,
    }

    impl ShutdownWatch {
        /// Blocks SIGINT/SIGTERM on this thread and opens the signalfd:
        /// call before any other thread spawns — every later thread
        /// inherits the block, so no thread can receive the default
        /// disposition.
        pub fn install() -> Result<Self> {
            let mask = SigSet::blocked(&[SIGINT, SIGTERM]);
            // SAFETY: the mask outlives the call; the block lands on
            // this thread and every thread it later spawns
            if unsafe { super::unix_imp::block_signals(&mask) } != 0 {
                return Err(anyhow!(
                    "cannot block SIGINT/SIGTERM: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: the fresh fd (-1), the same blocked mask, and no
            // flags — the wrapper owns the conversion
            let fd = unsafe { signalfd(-1, &raw const mask, 0) };
            if fd < 0 {
                return Err(anyhow!(
                    "cannot open the signalfd: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: the fresh descriptor the call above handed us;
            // a refused flip keeps the fd blocking and the run answers
            // the refusal below
            if unsafe { fcntl(fd, F_SETFL, O_NONBLOCK) } < 0 {
                return Err(anyhow!(
                    "cannot arm the signalfd: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: signalfd answered a fresh descriptor we own
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }

        /// Registers the signalfd as the reactor's own interest under
        /// the reserved shutdown token; its wake is the shutdown event,
        /// never a wait.
        pub fn arm(&self, vm: &mut ds_vm::Vm) -> Result<()> {
            vm.watch_shutdown(ds_vm::reactor::IoHandle(
                u64::try_from(self.fd.as_raw_fd()).expect("an fd fits the reactor's handle space"),
            ))
            .map_err(|err| anyhow!("the reactor refused the shutdown interest: {}", err.0))
        }

        /// The exit code the host answers for an observed shutdown:
        /// Unix reads the signalfd's siginfo and answers 128+signum —
        /// `None` when no signal arrived.
        pub fn exit_code(&self) -> Option<ExitCode> {
            let mut info = [0; 128];
            // SAFETY: the borrowed fd belongs to the owned handle
            // below; the ManuallyDrop keeps the borrow from closing
            // it, the OwnedFd remaining the owner. The fd is
            // non-blocking: no signal standing, the read answers
            // EAGAIN and `None` — never a park
            let mut file =
                std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(self.fd.as_raw_fd()) });
            let got = file.read(&mut info).ok()?;
            if got < 4 {
                return None;
            }
            let signum = u32::from_ne_bytes([info[0], info[1], info[2], info[3]]);
            Some(ExitCode::from(
                u8::try_from(128 + signum).unwrap_or(u8::MAX),
            ))
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    use std::{
        ffi::c_int,
        process::ExitCode,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicI32, Ordering},
        },
    };

    use anyhow::{Result, anyhow};

    use super::unix_imp::{SIGINT, SIGTERM, SigSet};

    unsafe extern "C" {
        fn sigwait(set: *const SigSet, sig: *mut c_int) -> c_int;
    }

    /// The host's blocking-drive helper: one sigwait thread holding the
    /// stop flag the blocking accept poll observes. The thread parks
    /// in sigwait until a signal arrives — the process's exit reaps
    /// it.
    pub struct ShutdownWatch {
        stop: Arc<AtomicBool>,
        signum: Arc<AtomicI32>,
    }

    impl ShutdownWatch {
        /// Blocks SIGINT/SIGTERM on this thread and spawns the sigwait
        /// helper, which inherits the block: call before any other
        /// thread spawns so no thread receives the default disposition.
        pub fn install() -> Result<Self> {
            let mask = SigSet::blocked(&[SIGINT, SIGTERM]);
            // SAFETY: the mask outlives the call
            if unsafe { super::unix_imp::block_signals(&mask) } != 0 {
                return Err(anyhow!(
                    "cannot block SIGINT/SIGTERM: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let stop = Arc::new(AtomicBool::new(false));
            let signum = Arc::new(AtomicI32::new(0));
            let worker_stop = Arc::clone(&stop);
            let worker_signum = Arc::clone(&signum);
            std::thread::spawn(move || {
                let mut sig = 0;
                // SAFETY: the mask outlives the call; the thread holds
                // the signal block it inherited
                if unsafe { sigwait(&mask, &mut sig) } == 0 {
                    worker_stop.store(true, Ordering::Relaxed);
                    worker_signum.store(sig, Ordering::Relaxed);
                }
            });
            Ok(Self { stop, signum })
        }

        /// Hands the VM the stop flag the helper flips: the blocking
        /// accept poll reads it between rounds and answers the
        /// shutdown trap — the reactor-less twin of the signalfd wake.
        pub fn arm(&self, vm: &mut ds_vm::Vm) -> Result<()> {
            vm.set_shutdown_flag(Arc::clone(&self.stop));
            Ok(())
        }

        /// The exit code the host answers for an observed shutdown:
        /// Unix answers 128+signum — `None` when no signal arrived.
        pub fn exit_code(&self) -> Option<ExitCode> {
            let signum = self.signum.load(Ordering::Relaxed);
            (signum > 0).then(|| ExitCode::from(u8::try_from(128 + signum).unwrap_or(u8::MAX)))
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        ffi::c_int,
        process::ExitCode,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicPtr, Ordering},
        },
    };

    use anyhow::{Result, anyhow};

    // the console-control handler reads one leaked flag pointer,
    // installed once per process before the handler registers
    static FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(c_int) -> c_int>,
            add: c_int,
        ) -> c_int;
    }

    /// The console-control handler: the control event flips the stop
    /// flag and reports the event handled — the default disposition
    /// (process death) never fires.
    unsafe extern "system" fn handler(_event: c_int) -> c_int {
        let flag = FLAG.load(Ordering::Relaxed);
        if !flag.is_null() {
            // SAFETY: the leaked flag lives for the process's lifetime
            unsafe { (*flag).store(true, Ordering::Relaxed) };
        }
        1 // TRUE: handled
    }

    /// The host's blocking-drive helper on Windows: the console-control
    /// event flips the stop flag the blocking accept poll observes.
    pub struct ShutdownWatch {
        stop: Arc<AtomicBool>,
    }

    impl ShutdownWatch {
        /// Registers the console-control handler. The handler's flag
        /// publishes BEFORE the handler registers, so a console event
        /// arriving between the two is never swallowed (the handler
        /// reporting handled over a null flag would drop it); call
        /// before any other thread spawns.
        pub fn install() -> Result<Self> {
            let stop = Arc::new(AtomicBool::new(false));
            // the leaked flag lives for the process's lifetime; the
            // handler reads the pointer, never dereferences past it
            FLAG.store(
                Arc::into_raw(Arc::clone(&stop)) as *mut AtomicBool,
                Ordering::Relaxed,
            );
            // SAFETY: the handler is a plain fn; registration is the
            // API's own contract
            if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
                return Err(anyhow!(
                    "cannot install the console-control handler: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { stop })
        }

        /// Hands the VM the stop flag the handler flips: the blocking
        /// accept poll reads it between rounds and answers the
        /// shutdown trap — the reactor-less twin of the signalfd wake.
        pub fn arm(&self, vm: &mut ds_vm::Vm) -> Result<()> {
            vm.set_shutdown_flag(Arc::clone(&self.stop));
            Ok(())
        }

        /// The exit code the host answers for an observed console event:
        /// the failed-run row — a console event owns no POSIX signum,
        /// and a graceful shutdown never reads as clean success.
        pub fn exit_code(&self) -> Option<ExitCode> {
            self.stop
                .load(Ordering::Relaxed)
                .then_some(ExitCode::from(1))
        }
    }
}

#[cfg(unix)]
mod unix_imp {
    //! The userspace sigset over the OS's own C symbols: glibc and
    //! musl agree the type is 128 bytes of signal bits, and both
    //! wrappers (pthread_sigmask, signalfd, sigwait) validate the size
    //! and convert to the kernel's 64-bit mask themselves.

    use std::{ffi::c_int, mem::size_of};

    pub const SIGINT: c_int = 2;
    pub const SIGTERM: c_int = 15;

    /// The process's signal mask shape (128 bits, one per signal).
    #[repr(C)]
    pub struct SigSet {
        bits: [u64; 16],
    }

    // the ABI both wrappers validate their size against
    const _: () = assert!(size_of::<SigSet>() == 128);

    impl SigSet {
        /// The set holding exactly the named signals.
        pub fn blocked(signals: &[c_int]) -> Self {
            let mut set = Self { bits: [0; 16] };
            for &sig in signals {
                // signal N is bit N-1 of the mask: word (N-1)/64
                set.bits[usize::try_from(sig - 1).expect("a signal id") / 64] |=
                    1u64 << ((sig - 1) % 64);
            }
            set
        }
    }

    unsafe extern "C" {
        fn pthread_sigmask(how: c_int, set: *const SigSet, oldset: *mut SigSet) -> c_int;
    }

    /// SIG_BLOCK, the pthread_sigmask operation that adds the set to
    /// the calling thread's mask.
    const SIG_BLOCK: c_int = 0;

    /// Adds `mask` to this thread's blocked set: 0 on success, the
    /// errno-shaped refusal otherwise. The block inherits to every
    /// thread spawned later.
    ///
    /// # Safety
    ///
    /// The caller owns `mask` for the call's duration.
    pub(super) unsafe fn block_signals(mask: &SigSet) -> c_int {
        // SAFETY: the mask outlives the call; no old set is requested
        unsafe { pthread_sigmask(SIG_BLOCK, mask, std::ptr::null_mut()) }
    }
}

pub use imp::ShutdownWatch;
