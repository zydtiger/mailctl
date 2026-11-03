//! Checks and requests the two macOS permissions mailctl needs: Full Disk
//! Access for reading the Mail store, and Automation for controlling Mail.
//!
//! macOS has no prompt for Full Disk Access, so the request opens its
//! System Settings pane and reveals this binary in Finder for the user to
//! drag in. Automation is queried through `AEDeterminePermissionToAutomateTarget`,
//! which can check without prompting and show the consent dialog on request.

use crate::output::{Output, PermissionReport, PermissionStatus};
use anyhow::{Context, Result, bail};
use std::ffi::{OsStr, c_void};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FULL_DISK_ACCESS_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
const AUTOMATION_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation";
const MAIL_BUNDLE_ID: &str = "com.apple.mail";
const POLL: Duration = Duration::from_secs(1);
const GRANT_TIMEOUT: Duration = Duration::from_secs(300);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

pub fn run(check: bool) -> Result<Output> {
    let binary = binary()?;
    let (full_disk_access, automation) = if check {
        (full_disk_access(), automation_status()?)
    } else {
        (request_full_disk_access(&binary)?, request_automation()?)
    };
    Ok(Output::Permissions(PermissionReport {
        binary,
        full_disk_access,
        automation,
    }))
}

/// The file macOS checks permissions against. `current_exe` returns the path
/// mailctl was started through, which may be a symlink such as a profile
/// link; macOS records grants for the file it resolves to.
pub fn binary() -> Result<PathBuf> {
    let path = std::env::current_exe().context("cannot locate the mailctl binary")?;
    Ok(path.canonicalize().unwrap_or(path))
}

fn full_disk_access() -> PermissionStatus {
    if can_read_mail() {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    }
}

/// Lists `~/Library/Mail` in a child process, which macOS checks against
/// mailctl's permissions. A fresh process sees a grant made while mailctl
/// waits, which a process that was already denied may not.
fn can_read_mail() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    Command::new("/bin/ls")
        .arg(Path::new(&home).join("Library").join("Mail"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn request_full_disk_access(binary: &Path) -> Result<PermissionStatus> {
    if can_read_mail() {
        return Ok(PermissionStatus::Granted);
    }
    eprintln!(
        "Full Disk Access: drag mailctl from the Finder window into the list in System Settings \
         and turn it on.\n  {}\nWaiting up to {} minutes...",
        binary.display(),
        GRANT_TIMEOUT.as_secs() / 60
    );
    open(&[OsStr::new("-R"), binary.as_os_str()])?;
    open(&[OsStr::new(FULL_DISK_ACCESS_PANE)])?;
    Ok(if wait_until(GRANT_TIMEOUT, can_read_mail) {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    })
}

fn automation_status() -> Result<PermissionStatus> {
    Ok(match automation(false)? {
        Access::Granted => PermissionStatus::Granted,
        Access::Denied => PermissionStatus::Denied,
        Access::NotDetermined => PermissionStatus::NotDetermined,
        // macOS answers only while Mail is running.
        Access::MailNotRunning => PermissionStatus::Unknown,
    })
}

fn request_automation() -> Result<PermissionStatus> {
    let mut access = automation(false)?;
    if access == Access::MailNotRunning {
        open(&[
            OsStr::new("-g"),
            OsStr::new("-b"),
            OsStr::new(MAIL_BUNDLE_ID),
        ])?;
        wait_until(LAUNCH_TIMEOUT, || {
            access = automation(false).unwrap_or(Access::MailNotRunning);
            access != Access::MailNotRunning
        });
    }
    match access {
        Access::Granted => Ok(PermissionStatus::Granted),
        Access::NotDetermined => {
            eprintln!("Automation: allow mailctl to control Mail in the dialog.");
            Ok(match automation(true)? {
                Access::Granted => PermissionStatus::Granted,
                _ => PermissionStatus::Denied,
            })
        }
        Access::Denied => {
            // macOS asks only once; after a refusal the switch is in Settings.
            eprintln!(
                "Automation: turn on Mail under mailctl in System Settings.\n\
                 Waiting up to {} minutes...",
                GRANT_TIMEOUT.as_secs() / 60
            );
            open(&[OsStr::new(AUTOMATION_PANE)])?;
            let granted = wait_until(GRANT_TIMEOUT, || {
                matches!(automation(false), Ok(Access::Granted))
            });
            Ok(if granted {
                PermissionStatus::Granted
            } else {
                PermissionStatus::Denied
            })
        }
        Access::MailNotRunning => bail!("Mail did not start; open Mail and run this again"),
    }
}

fn open(args: &[&OsStr]) -> Result<()> {
    let status = Command::new("/usr/bin/open")
        .args(args)
        .status()
        .context("cannot run open")?;
    if !status.success() {
        bail!("open {:?} failed", args);
    }
    Ok(())
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Granted,
    Denied,
    NotDetermined,
    MailNotRunning,
}

const fn four_char_code(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const TYPE_APPLICATION_BUNDLE_ID: u32 = four_char_code(b"bund");
const TYPE_WILDCARD: u32 = four_char_code(b"****");
const NO_ERR: i32 = 0;
const PROC_NOT_FOUND: i32 = -600;
const ERR_AE_EVENT_NOT_PERMITTED: i32 = -1743;
const ERR_AE_EVENT_WOULD_REQUIRE_USER_CONSENT: i32 = -1744;

#[repr(C)]
struct AEDesc {
    descriptor_type: u32,
    data_handle: *mut c_void,
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn AECreateDesc(type_code: u32, data: *const c_void, size: isize, result: *mut AEDesc) -> i16;
    fn AEDisposeDesc(desc: *mut AEDesc) -> i16;
    fn AEDeterminePermissionToAutomateTarget(
        target: *const AEDesc,
        event_class: u32,
        event_id: u32,
        ask_user_if_needed: u8,
    ) -> i32;
}

/// Asks macOS whether mailctl may send Apple Events to Mail. With `ask`,
/// macOS shows the consent dialog when the user has not decided yet and
/// blocks until they answer.
fn automation(ask: bool) -> Result<Access> {
    let bundle_id = MAIL_BUNDLE_ID.as_bytes();
    let mut target = AEDesc {
        descriptor_type: 0,
        data_handle: std::ptr::null_mut(),
    };
    let created = unsafe {
        AECreateDesc(
            TYPE_APPLICATION_BUNDLE_ID,
            bundle_id.as_ptr().cast(),
            bundle_id.len() as isize,
            &mut target,
        )
    };
    if created != 0 {
        bail!("cannot address Mail: AECreateDesc returned {created}");
    }
    let status = unsafe {
        AEDeterminePermissionToAutomateTarget(&target, TYPE_WILDCARD, TYPE_WILDCARD, ask.into())
    };
    unsafe { AEDisposeDesc(&mut target) };
    Ok(match status {
        NO_ERR => Access::Granted,
        ERR_AE_EVENT_NOT_PERMITTED => Access::Denied,
        ERR_AE_EVENT_WOULD_REQUIRE_USER_CONSENT => Access::NotDetermined,
        PROC_NOT_FOUND => Access::MailNotRunning,
        other => bail!("cannot check Automation permission for Mail: OSStatus {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_char_codes_are_big_endian() {
        assert_eq!(TYPE_APPLICATION_BUNDLE_ID, 0x6275_6e64);
        assert_eq!(TYPE_WILDCARD, 0x2a2a_2a2a);
    }
}
