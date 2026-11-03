//! Makes mailctl its own TCC responsible process.
//!
//! macOS attributes privacy checks to the responsible process, which for a
//! command-line tool is normally the app that launched it: a terminal, an
//! editor, or an agent app. Full Disk Access and Automation for Mail would
//! then have to be granted to every such app, and to whatever helper an agent
//! app runs commands through. Re-executing this binary with responsibility
//! disclaimed makes mailctl the responsible process, so both permissions are
//! granted to mailctl once, and the `osascript` it starts inherits them.

use std::ffi::{CString, c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

const DISCLAIMED_ENV: &str = "MAILCTL_RESPONSIBILITY_DISCLAIMED";

type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int;

pub fn is_disclaimed() -> bool {
    std::env::var_os(DISCLAIMED_ENV).is_some()
}

/// Runs mailctl again in a disclaimed child and returns the child's exit code.
/// Returns `None` when this process is already disclaimed or the child cannot
/// be started, in which case the caller does the work itself.
pub fn reexec_disclaimed() -> Option<ExitCode> {
    if is_disclaimed() {
        return None;
    }
    let set_disclaim = lookup_set_disclaim()?;
    let program = CString::new(std::env::current_exe().ok()?.as_os_str().as_bytes()).ok()?;
    let args = std::env::args_os()
        .map(|arg| CString::new(arg.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let mut env = std::env::vars_os()
        .filter(|(key, _)| key != DISCLAIMED_ENV)
        .map(|(key, value)| {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry)
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    env.push(CString::new(format!("{DISCLAIMED_ENV}=1")).ok()?);

    let pid = spawn(set_disclaim, &program, &args, &env)?;
    Some(wait_for_child(pid))
}

fn lookup_set_disclaim() -> Option<SetDisclaim> {
    // Private libSystem symbol; resolved at run time so a missing symbol
    // falls back to running in-process instead of failing to load.
    let symbol = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"responsibility_spawnattrs_setdisclaim".as_ptr(),
        )
    };
    if symbol.is_null() {
        return None;
    }
    Some(unsafe { std::mem::transmute::<*mut libc::c_void, SetDisclaim>(symbol) })
}

fn spawn(
    set_disclaim: SetDisclaim,
    program: &CString,
    args: &[CString],
    env: &[CString],
) -> Option<libc::pid_t> {
    let argv = null_terminated(args);
    let envp = null_terminated(env);
    let mut pid: libc::pid_t = 0;
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return None;
        }
        let status = if set_disclaim(&mut attr, 1) == 0 {
            libc::posix_spawn(
                &mut pid,
                program.as_ptr(),
                std::ptr::null(),
                &attr,
                argv.as_ptr(),
                envp.as_ptr(),
            )
        } else {
            -1
        };
        libc::posix_spawnattr_destroy(&mut attr);
        (status == 0).then_some(pid)
    }
}

fn null_terminated(strings: &[CString]) -> Vec<*mut c_char> {
    strings
        .iter()
        .map(|string| string.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect()
}

fn wait_for_child(pid: libc::pid_t) -> ExitCode {
    // The child shares the terminal's process group and receives Ctrl-C and
    // Ctrl-\ itself; the parent only reports how it ended.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
    let mut status: c_int = 0;
    loop {
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            break;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return ExitCode::FAILURE;
        }
    }
    if libc::WIFEXITED(status) {
        return ExitCode::from(libc::WEXITSTATUS(status) as u8);
    }
    let signal = libc::WTERMSIG(status);
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
    ExitCode::from(128 + signal as u8)
}
