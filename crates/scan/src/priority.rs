//! Thread priority helpers.

/// Lowers the calling thread to idle I/O priority and nice 19, so scans never make the desktop
/// stutter. Both settings are per thread on Linux; failures are ignored (lowering priority never
/// needs privileges, but the syscalls may be filtered in sandboxes).
pub fn lower_thread_priority() {
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    let tid = rustix::thread::gettid();
    // SAFETY: ioprio_set takes three integer arguments and has no memory-safety preconditions;
    // `who` is the calling thread’s id.
    #[allow(unsafe_code)]
    unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            libc::c_long::from(tid.as_raw_nonzero().get()),
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        );
    }
    let _ = rustix::process::setpriority_process(Some(tid), 19);
}
