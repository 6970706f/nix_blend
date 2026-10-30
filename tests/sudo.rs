//! Tests of the elevated code paths. They need a user namespace with subordinate
//! IDs and a NixOS system profile to replace, so they cannot run everywhere. When
//! they cannot run they say so, and fail instead if
//! `NIXBLEND_REQUIRE_SUDO_TESTS` is set.

use std::env;
use std::path::Path;
use std::process::Command;

const NAMESPACE_ARGS: [&str; 5] = [
    "--user",
    "--mount",
    "--map-root-user",
    "--map-users=auto",
    "--map-groups=auto",
];

fn namespace_available() -> bool {
    Path::new("/run/current-system/sw/bin").is_dir()
        && Command::new("unshare")
            .args(NAMESPACE_ARGS)
            .arg("true")
            .output()
            .is_ok_and(|output| output.status.success())
}

#[test]
fn elevated_scenarios() {
    if !namespace_available() {
        assert!(
            env::var_os("NIXBLEND_REQUIRE_SUDO_TESTS").is_none(),
            "the elevated scenarios cannot run on this system"
        );
        eprintln!("SKIPPED elevated scenarios: user namespaces or NixOS profile unavailable");
        return;
    }

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support_sudo_scenarios.sh");
    let output = Command::new("unshare")
        .args(NAMESPACE_ARGS)
        .arg("sh")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_nixblend"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("ALL SCENARIOS PASSED"),
        "{stdout}\n{stderr}"
    );
}
