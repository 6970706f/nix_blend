//! Identification of the original user and temporary privilege control.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use rustix::process::{Gid, Uid, getegid, geteuid, getgroups, getuid};
use rustix::thread::{set_thread_groups, set_thread_res_gid, set_thread_res_uid};

use super::tools;
use crate::application::error::UserError;

const PASSWD_PATH: &str = "/etc/passwd";
const GETENT: &str = "getent";

/// User who owns the Nixblend data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// Tells whether the process is running with root privileges.
pub fn is_root() -> bool {
    geteuid().is_root()
}

/// Resolves the user who started the command, even under `sudo`.
pub fn current() -> Result<UserIdentity, UserError> {
    let passwd = fs::read_to_string(PASSWD_PATH).map_err(UserError::Database)?;
    let sudo_user = env::var_os("SUDO_USER")
        .map(|value| value.into_string().map_err(|_| UserError::InvalidSudoUser))
        .transpose()?;
    // Lossy on purpose: an unreadable value must fail the comparison, not skip it.
    let sudo_uid = env::var_os("SUDO_UID").map(|value| value.to_string_lossy().into_owned());
    let (ruid, euid) = (getuid().as_raw(), geteuid().as_raw());
    let resolve_from = |database: &str| {
        resolve(
            ruid,
            euid,
            sudo_user.as_deref(),
            sudo_uid.as_deref(),
            database,
        )
    };

    match resolve_from(&passwd) {
        Err(UserError::UnknownUser(name)) => directory_entry(&name, euid == 0)
            .map_or(Err(UserError::UnknownUser(name)), |entry| {
                resolve_from(&entry)
            }),
        Err(UserError::UnknownUid(uid)) => directory_entry(&uid.to_string(), euid == 0)
            .map_or(Err(UserError::UnknownUid(uid)), |entry| {
                resolve_from(&entry)
            }),
        result => result,
    }
}

/// Asks the system's name service for an entry missing from `/etc/passwd`, which
/// covers users from LDAP or `systemd-homed`. Any failure means "no entry".
fn directory_entry(key: &str, elevated: bool) -> Option<String> {
    let mut command = Command::new(tools::program(GETENT, elevated).ok()?);
    tools::apply_environment(&mut command, elevated, None);
    let output = command
        .args(["passwd", key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

/// `SUDO_USER` is only considered when the real and effective UIDs are both 0,
/// which is how `sudo` leaves the process. In a setuid binary the environment
/// is controlled by the caller and must not decide who owns the data.
fn resolve(
    ruid: u32,
    euid: u32,
    sudo_user: Option<&str>,
    sudo_uid: Option<&str>,
    passwd: &str,
) -> Result<UserIdentity, UserError> {
    let Some(name) = sudo_user.filter(|_| ruid == 0 && euid == 0) else {
        return passwd_entries(passwd)
            .find(|(_, user)| user.uid == ruid)
            .map(|(_, user)| user)
            .ok_or(UserError::UnknownUid(ruid));
    };

    let user = passwd_entries(passwd)
        .find(|(entry_name, _)| *entry_name == name)
        .map(|(_, user)| user)
        .ok_or_else(|| UserError::UnknownUser(name.to_owned()))?;
    if sudo_uid.and_then(|uid| uid.parse().ok()) != Some(user.uid) {
        return Err(UserError::SudoUidMismatch(name.to_owned()));
    }
    Ok(user)
}

/// Iterates over the well-formed entries of a `passwd`-format file.
fn passwd_entries(passwd: &str) -> impl Iterator<Item = (&str, UserIdentity)> {
    passwd.lines().filter_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let uid = fields.next()?.parse().ok()?;
        let gid = fields.next()?.parse().ok()?;
        let _gecos = fields.next()?;
        let home = PathBuf::from(fields.next()?);
        // -1 is reserved by the kernel and does not identify a user.
        let valid = uid != u32::MAX && gid != u32::MAX && home.is_absolute();
        valid.then_some((name, UserIdentity { uid, gid, home }))
    })
}

/// While it exists, the process accesses files with the identity of the original
/// user; when dropped, the previous privileges are restored.
///
/// This way the kernel itself prevents symlinks in the home directory from
/// leading root to write outside it, and new files get the right owner from the
/// start. The calls only affect the current thread, which is the one doing file
/// access: other threads only read pipes or clean up temporary files. Supplementary
/// groups are reduced to the primary group, since they are root's otherwise.
pub struct PrivilegeGuard {
    restore: Option<(Uid, Gid, Vec<Gid>)>,
}

/// Assumes the identity of `user` if the process is running as root.
pub fn drop_privileges(user: &UserIdentity) -> Result<PrivilegeGuard, UserError> {
    let euid = geteuid();
    let egid = getegid();
    if !euid.is_root() || user.uid == 0 {
        return Ok(PrivilegeGuard { restore: None });
    }

    let groups = getgroups().map_err(|error| UserError::Privileges(error.into()))?;
    set_effective_ids(Uid::from_raw(user.uid), Gid::from_raw(user.gid), euid, egid)
        .map_err(UserError::Privileges)?;
    Ok(PrivilegeGuard {
        restore: Some((euid, egid, groups)),
    })
}

/// Changes the GID before the UID: after giving up UID 0, switching to an
/// arbitrary GID would no longer be allowed.
fn set_effective_ids(uid: Uid, gid: Gid, saved_uid: Uid, saved_gid: Gid) -> io::Result<()> {
    set_thread_groups(&[gid])?;
    set_thread_res_gid(None, gid, saved_gid)?;
    if let Err(error) = set_thread_res_uid(None, uid, saved_uid) {
        let _ = set_thread_res_gid(None, saved_gid, saved_gid);
        return Err(error.into());
    }
    Ok(())
}

impl Drop for PrivilegeGuard {
    fn drop(&mut self) {
        if let Some((uid, gid, groups)) = &self.restore {
            // If restoring fails, the process simply stays unprivileged and later
            // operations fail with a permission error.
            let _ = set_thread_res_uid(None, *uid, *uid);
            let _ = set_thread_groups(groups);
            let _ = set_thread_res_gid(None, *gid, *gid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:System administrator:/root:/bin/sh\n\
        alice:x:1000:100:Alice:/home/alice:/bin/zsh\n\
        malformed line\n\
        relative:x:1001:100::home/relative:/bin/sh\n";

    fn alice() -> UserIdentity {
        UserIdentity {
            uid: 1000,
            gid: 100,
            home: PathBuf::from("/home/alice"),
        }
    }

    #[test]
    fn unprivileged_process_uses_its_real_uid() {
        assert_eq!(resolve(1000, 1000, None, None, PASSWD).unwrap(), alice());
    }

    #[test]
    fn unprivileged_process_ignores_sudo_variables() {
        let user = resolve(1000, 1000, Some("root"), Some("0"), PASSWD).unwrap();
        assert_eq!(user, alice());
    }

    #[test]
    fn setuid_process_does_not_trust_sudo_user() {
        let user = resolve(1000, 0, Some("root"), Some("0"), PASSWD).unwrap();
        assert_eq!(user, alice());
    }

    #[test]
    fn sudo_resolves_the_original_user() {
        let user = resolve(0, 0, Some("alice"), Some("1000"), PASSWD).unwrap();
        assert_eq!(user, alice());
    }

    #[test]
    fn root_without_sudo_is_root() {
        assert_eq!(resolve(0, 0, None, None, PASSWD).unwrap().uid, 0);
    }

    #[test]
    fn sudo_uid_is_required() {
        let error = resolve(0, 0, Some("alice"), None, PASSWD).unwrap_err();
        assert!(matches!(error, UserError::SudoUidMismatch(_)));
    }

    #[test]
    fn sudo_user_must_exist() {
        let error = resolve(0, 0, Some("mallory"), None, PASSWD).unwrap_err();
        assert!(matches!(error, UserError::UnknownUser(_)));
    }

    #[test]
    fn sudo_uid_must_match_the_database() {
        let error = resolve(0, 0, Some("alice"), Some("0"), PASSWD).unwrap_err();
        assert!(matches!(error, UserError::SudoUidMismatch(_)));
    }

    #[test]
    fn unreadable_sudo_uid_fails_the_comparison() {
        let error = resolve(0, 0, Some("alice"), Some("\u{fffd}"), PASSWD).unwrap_err();
        assert!(matches!(error, UserError::SudoUidMismatch(_)));
    }

    #[test]
    fn directory_entries_use_the_passwd_format() {
        let entry = "ldapuser:*:5000:5000:LDAP:/home/ldapuser:/bin/sh\n";
        let user = resolve(5000, 5000, None, None, entry).unwrap();
        assert_eq!(user.home, PathBuf::from("/home/ldapuser"));
    }

    #[test]
    fn unknown_uid_is_an_error() {
        let error = resolve(4242, 4242, None, None, PASSWD).unwrap_err();
        assert!(matches!(error, UserError::UnknownUid(4242)));
    }

    #[test]
    fn entries_with_relative_home_are_ignored() {
        let error = resolve(1001, 1001, None, None, PASSWD).unwrap_err();
        assert!(matches!(error, UserError::UnknownUid(1001)));
    }
}
