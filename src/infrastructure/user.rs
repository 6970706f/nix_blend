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
const GROUP_PATH: &str = "/etc/group";
const GETENT: &str = "getent";

/// User who owns the Nixblend data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    /// All groups of the user, known only when the process is elevated and has to act
    /// as that user.
    pub groups: Vec<u32>,
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

    let mut user = match resolve_from(&passwd) {
        Err(UserError::UnknownUser(name)) => lookup(&["passwd", &name], euid == 0)
            .map_or(Err(UserError::UnknownUser(name)), |entry| {
                resolve_from(&entry)
            }),
        Err(UserError::UnknownUid(uid)) => lookup(&["passwd", &uid.to_string()], euid == 0)
            .map_or(Err(UserError::UnknownUid(uid)), |entry| {
                resolve_from(&entry)
            }),
        result => result,
    }?;
    if euid == 0 && user.uid != 0 {
        user.groups = groups_of(&user);
    }
    Ok(user)
}

/// All groups of an elevated process's original user, through the name service if
/// possible. The primary group is always there, which is the safe minimum.
fn groups_of(user: &UserIdentity) -> Vec<u32> {
    let listed = lookup(&["initgroups", &user.name], true)
        .map(|entry| parse_initgroups(&entry))
        .or_else(|| {
            let groups = fs::read_to_string(GROUP_PATH).ok()?;
            Some(parse_group_file(&groups, &user.name))
        })
        .unwrap_or_default();
    let mut groups = vec![user.gid];
    groups.extend(listed.into_iter().filter(|gid| *gid != user.gid));
    groups
}

/// Reads `name   gid gid ...`, as printed by `getent initgroups`.
fn parse_initgroups(entry: &str) -> Vec<u32> {
    entry
        .split_whitespace()
        .skip(1)
        .filter_map(|gid| gid.parse().ok())
        .collect()
}

/// Finds the groups listing `name` in a file in the `group` format.
fn parse_group_file(groups: &str, name: &str) -> Vec<u32> {
    groups
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let _group_name = fields.next()?;
            let _password = fields.next()?;
            let gid = fields.next()?.parse().ok()?;
            let members = fields.next()?;
            members
                .split(',')
                .any(|member| member == name)
                .then_some(gid)
        })
        .collect()
}

/// Asks the system's name service, which covers users from LDAP or `systemd-homed`. Any
/// failure means "no entry".
fn lookup(arguments: &[&str], elevated: bool) -> Option<String> {
    let mut command = Command::new(tools::program(GETENT, elevated).ok()?);
    tools::apply_environment(&mut command, elevated, None);
    let output = command
        .args(arguments)
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
        valid.then_some((
            name,
            UserIdentity {
                name: name.to_owned(),
                uid,
                gid,
                home,
                groups: Vec::new(),
            },
        ))
    })
}

/// While it exists, the process accesses files with the identity of the original
/// user; when dropped, the previous privileges are restored.
///
/// This way the kernel itself prevents symlinks in the home directory from
/// leading root to write outside it, and new files get the right owner from the
/// start. The calls only affect the current thread, which is the one doing file
/// access: other threads only read pipes or clean up temporary files. The groups are
/// those of the user, since they are root's otherwise.
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

    let saved_groups = getgroups().map_err(|error| UserError::Privileges(error.into()))?;
    let groups: Vec<Gid> = if user.groups.is_empty() {
        vec![Gid::from_raw(user.gid)]
    } else {
        user.groups.iter().map(|gid| Gid::from_raw(*gid)).collect()
    };
    set_effective_ids(
        Uid::from_raw(user.uid),
        Gid::from_raw(user.gid),
        &groups,
        euid,
        egid,
    )
    .map_err(UserError::Privileges)?;
    Ok(PrivilegeGuard {
        restore: Some((euid, egid, saved_groups)),
    })
}

/// Changes the GID before the UID: after giving up UID 0, switching to an
/// arbitrary GID would no longer be allowed.
fn set_effective_ids(
    uid: Uid,
    gid: Gid,
    groups: &[Gid],
    saved_uid: Uid,
    saved_gid: Gid,
) -> io::Result<()> {
    set_thread_groups(groups)?;
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
            name: "alice".to_owned(),
            uid: 1000,
            gid: 100,
            home: PathBuf::from("/home/alice"),
            groups: Vec::new(),
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
    fn group_sources_are_parsed() {
        assert_eq!(parse_initgroups("alice   100 1 57\n"), [100, 1, 57]);
        assert_eq!(
            parse_group_file(
                "root:x:0:\nwheel:x:1:bob,alice\naudio:x:2:bob\nmalformed\n",
                "alice"
            ),
            [1]
        );
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
