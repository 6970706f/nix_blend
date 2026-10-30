//! The interactive interface in a real pseudo-terminal, through `script`, with simulated
//! tools in an isolated `PATH`. No test touches the real system configuration.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// See the same lock in `tests/install.rs`.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

const MODULE: &str = "{ pkgs, ... }:\n{\n  home.packages = with pkgs; [\n    git\n  ];\n}\n";
const FOO: &str = r#"{"foo":{"name":"foo-1.0","pname":"foo","version":"1.0","description":"Foo tool","homepage":"https://foo.example","license":"MIT","available":true,"broken":false,"unfree":false,"insecure":false},"pkgs.foo":null}"#;
const SEARCH: &str =
    r#"{"legacyPackages.x.foo":{"pname":"foo","version":"1.0","description":"Foo tool"}}"#;
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
const TIMEOUT: Duration = Duration::from_secs(20);

fn which(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// The tools the sessions need, or `None` where this cannot run.
fn tools() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let found = (which("script")?, which("stty")?, which("sh")?);
    Some(found)
}

macro_rules! require_tools {
    () => {
        match tools() {
            Some(tools) => tools,
            None => {
                eprintln!("SKIPPED: script, stty or sh is not available");
                return;
            }
        }
    };
}

struct Session {
    child: Child,
    input: std::process::ChildStdin,
    output: Arc<Mutex<Vec<u8>>>,
    finished: mpsc::Receiver<()>,
}

impl Session {
    /// Waits until the screen has shown `text`.
    fn wait_for(&self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            // The interface only sends cells that changed, so whole words are only found
            // on the screen. What the rebuild prints goes to the plain output.
            let screen = screen_of(&self.text());
            if screen.contains(text) || self.text().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "'{text}' never appeared on:\n{screen}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn text(&self) -> String {
        let bytes = self.output.lock().unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn send(&mut self, keys: &str) {
        self.input.write_all(keys.as_bytes()).unwrap();
        self.input.flush().unwrap();
        // The interface reads a key at a time, and what a key does takes a moment to show.
        thread::sleep(Duration::from_millis(200));
    }

    /// Waits for the program to end and returns its exit code.
    fn finish(mut self) -> Option<i32> {
        self.finished.recv_timeout(TIMEOUT).expect("it did not end");
        self.child.wait().unwrap().code()
    }
}

struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        assert!(
            !rustix::process::geteuid().is_root(),
            "these tests must not run as root"
        );
        let sandbox = Self {
            root: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(sandbox.bin()).unwrap();
        fs::write(sandbox.packages(), MODULE).unwrap();
        sandbox.tool("nix-instantiate", "exit 0");
        sandbox.tool(
            "nix",
            &format!(
                r#"case "$*" in
  *attrNames*) echo '[]';;
  *" eval "*) echo '{FOO}';;
  *" search "*) echo '{SEARCH}';;
esac"#
            ),
        );
        sandbox.tool(
            "sudo",
            &format!(
                "echo rebuild-ran-here\necho \"sudo $*\" >> '{}'",
                sandbox.log().display()
            ),
        );
        sandbox
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn packages(&self) -> PathBuf {
        self.root.path().join("packages.nix")
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("commands.log")
    }

    fn data_home(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn tool(&self, name: &str, body: &str) {
        let _lock = SPAWN_LOCK.lock().unwrap();
        let path = self.bin().join(name);
        let shell = which("sh").unwrap();
        fs::write(&path, format!("#!{}\n{body}\n", shell.display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_config(&self) {
        let directory = self.data_home().join("nixblend");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("config.toml"),
            format!("packages_file = \"{}\"\n", self.packages().display()),
        )
        .unwrap();
    }

    fn content(&self) -> String {
        fs::read_to_string(self.packages()).unwrap()
    }

    fn commands(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn temporary_files(&self) -> Vec<String> {
        fs::read_dir(self.root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".nixblend-") && !name.ends_with("-backup"))
            .collect()
    }

    /// Starts the interface in a pseudo-terminal of 100x30.
    fn open(&self, (script, stty, sh): &(PathBuf, PathBuf, PathBuf)) -> Session {
        let line = format!(
            "'{}' cols 100 rows 30; exec env PATH='{}' XDG_DATA_HOME='{}' TERM=xterm-256color LANG=en_US.UTF-8 '{}'",
            stty.display(),
            self.bin().display(),
            self.data_home().display(),
            env!("CARGO_BIN_EXE_nixblend"),
        );
        let mut child = {
            let _lock = SPAWN_LOCK.lock().unwrap();
            Command::new(script)
                .args([
                    "--quiet",
                    "--return",
                    "--flush",
                    "--command",
                    &line,
                    "/dev/null",
                ])
                .env("SHELL", sh)
                .env_remove("SUDO_USER")
                .env_remove("SUDO_UID")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let input = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&output);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(count) = stdout.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                writer.lock().unwrap().extend_from_slice(&buffer[..count]);
            }
            let _ = done.send(());
        });
        Session {
            child,
            input,
            output,
            finished,
        }
    }
}

fn restored(session: &Session) -> bool {
    let text = session.text();
    let entered = text.rfind(ENTER_ALTERNATE_SCREEN);
    let left = text.rfind(LEAVE_ALTERNATE_SCREEN);
    matches!((entered, left), (Some(entered), Some(left)) if left > entered)
}

#[test]
fn opens_lists_and_quits_restoring_the_terminal() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let mut session = sandbox.open(&tools);

    session.wait_for("NIXBLEND");
    session.wait_for("DECLARED");
    session.wait_for("git");
    session.send("q");

    assert!(restored(&session), "the terminal was not restored");
    assert_eq!(session.finish(), Some(0));
}

#[test]
fn ctrl_c_quits_from_a_text_field_and_restores_the_terminal() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let mut session = sandbox.open(&tools);

    session.wait_for("DECLARED");
    session.send("/iq");
    session.wait_for("iq_");
    session.send("\u{3}");

    assert!(restored(&session));
    assert_eq!(session.finish(), Some(130));
}

#[test]
fn being_terminated_restores_the_terminal() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let session = sandbox.open(&tools);
    session.wait_for("DECLARED");

    // `script` has one child: the program. Other sessions run in parallel.
    let script = session.child.id();
    let children = fs::read_to_string(format!("/proc/{script}/task/{script}/children")).unwrap();
    let program: i32 = children.split_whitespace().next().unwrap().parse().unwrap();
    let pid = rustix::process::Pid::from_raw(program).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !restored(&session) {
        assert!(Instant::now() < deadline, "the terminal was not restored");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn search_install_with_diff_and_rebuild_through_the_use_cases() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let mut session = sandbox.open(&tools);

    session.wait_for("DECLARED");
    session.send("/foo\r");
    session.wait_for("RESULTS");
    session.wait_for("Foo tool");
    session.send("i");
    session.wait_for("Apply this change?");
    session.wait_for("+    foo");
    assert_eq!(
        sandbox.content(),
        MODULE,
        "nothing changes before the answer"
    );

    session.send("y");
    session.wait_for("Run the NixOS rebuild now?");
    assert!(sandbox.content().contains("    foo\n"));
    assert!(
        fs::metadata(sandbox.root.path().join(".packages.nix.nixblend-backup")).is_ok(),
        "the use case keeps its backup"
    );

    session.send("y");
    session.wait_for("rebuild-ran-here");
    session.wait_for("Rebuild: done.");
    assert!(sandbox.commands().contains("sudo nixos-rebuild switch"));

    session.send("\r");
    session.send("q");
    assert!(restored(&session));
    assert_eq!(session.finish(), Some(0));
    assert!(sandbox.temporary_files().is_empty());
}

#[test]
fn refusing_the_diff_changes_nothing() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let mut session = sandbox.open(&tools);

    session.wait_for("DECLARED");
    session.send("/foo\r");
    session.wait_for("Foo tool");
    session.send("i");
    session.wait_for("Apply this change?");
    session.send("n");
    session.wait_for("Edit cancelled");
    assert_eq!(sandbox.content(), MODULE);
    assert_eq!(sandbox.commands(), "");

    session.send("\r");
    session.send("q");
    assert_eq!(session.finish(), Some(0));
}

#[test]
fn removal_shows_the_diff_and_uses_the_use_case() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let mut session = sandbox.open(&tools);

    session.wait_for("git");
    session.send("r");
    session.wait_for("REMOVE");
    session.wait_for("-    git");
    session.wait_for("Apply this change?");
    session.send("y");
    session.wait_for("Run the NixOS rebuild now?");
    session.send("n");
    session.wait_for("Rebuild: not run");
    assert!(!sandbox.content().contains("git"));

    session.send("\r");
    session.send("q");
    assert_eq!(session.finish(), Some(0));
}

#[test]
fn a_failed_rebuild_waits_and_offers_to_undo() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("sudo", "echo rebuild-exploded\nexit 7");
    let mut session = sandbox.open(&tools);

    session.wait_for("git");
    session.send("r");
    session.wait_for("Apply this change?");
    session.send("y");
    session.wait_for("Run the NixOS rebuild now?");
    session.send("y");
    session.wait_for("rebuild-exploded");
    session.wait_for("Press Enter to go back");
    session.send("\r");
    session.wait_for("Undo the edit?");
    session.send("y");
    session.wait_for("back to its previous content");
    assert_eq!(sandbox.content(), MODULE);

    session.send("\r");
    session.send("q");
    assert_eq!(session.finish(), Some(0));
}

#[test]
fn a_failure_shows_the_real_cause() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix", "echo 'error: could not resolve host' >&2\nexit 1");
    let mut session = sandbox.open(&tools);

    session.wait_for("DECLARED");
    session.send("/foo\r");
    session.wait_for("could not resolve host");
    session.send("q");
    assert_eq!(session.finish(), Some(0));
}

#[test]
fn the_first_install_asks_for_the_file() {
    let tools = require_tools!();
    let sandbox = Sandbox::new();
    let mut session = sandbox.open(&tools);

    session.wait_for("No file is managed yet");
    session.send("/foo\r");
    session.wait_for("Foo tool");
    session.send("i");
    session.wait_for("Path to the file to manage");
    session.send(&format!("{}\r", sandbox.packages().display()));
    session.wait_for("Apply this change?");
    assert!(
        fs::metadata(sandbox.data_home().join("nixblend/config.toml")).is_ok(),
        "the path was saved once it proved usable"
    );
    session.send("n");
    session.send("\r");
    session.send("q");
    assert_eq!(session.finish(), Some(0));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn esc_cancels_a_slow_search_and_interruption_leaves_no_files() {
    let tools = require_tools!();
    let sleep = which("sleep").unwrap();
    let sandbox = Sandbox::new();
    let marker = sandbox.root.path().join("tool.pid");
    sandbox.write_config();
    sandbox.tool(
        "nix",
        &format!(
            r#"case "$*" in
  *attrNames*) echo '[]';;
  *" eval "*) echo '{{}}';;
  *" search "*) echo $$ > '{}'; {} 60 & wait;;
esac"#,
            marker.display(),
            sleep.display()
        ),
    );
    let mut session = sandbox.open(&tools);

    session.wait_for("DECLARED");
    session.send("/foo\r");
    session.wait_for("Searching...");
    let started = Instant::now();
    let deadline = Instant::now() + TIMEOUT;
    while fs::read_to_string(&marker).map_or(true, |pid| pid.trim().is_empty()) {
        assert!(Instant::now() < deadline, "the tool did not start");
        thread::sleep(Duration::from_millis(20));
    }
    let tool = PathBuf::from("/proc").join(fs::read_to_string(&marker).unwrap().trim());

    session.send("\u{1b}");
    session.wait_for("DECLARED");
    let deadline = Instant::now() + Duration::from_secs(5);
    while tool.exists() {
        assert!(Instant::now() < deadline, "the search is still running");
        thread::sleep(Duration::from_millis(20));
    }
    session.send("q");
    assert_eq!(session.finish(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(sandbox.temporary_files().is_empty());
}

#[test]
fn without_a_terminal_it_explains_instead_of_drawing() {
    let sandbox = Sandbox::new();
    let output = Command::new(env!("CARGO_BIN_EXE_nixblend"))
        .env("PATH", sandbox.bin())
        .env("XDG_DATA_HOME", sandbox.data_home())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("needs a terminal"), "{error}");
    assert!(output.stdout.is_empty());
}

const ROWS: usize = 30;
const COLUMNS: usize = 100;

/// What a terminal of 100x30 would show after this output: just enough of a terminal for
/// what the interface sends, which is cursor positions, styles, and text.
fn screen_of(output: &str) -> String {
    let mut grid = vec![vec![' '; COLUMNS]; ROWS];
    let (mut row, mut column) = (0usize, 0usize);
    let mut characters = output.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\x1b' => {
                if characters.next_if_eq(&'[').is_none() {
                    continue;
                }
                let mut parameters = String::new();
                let mut command = None;
                for next in characters.by_ref() {
                    if next.is_ascii_alphabetic() {
                        command = Some(next);
                        break;
                    }
                    parameters.push(next);
                }
                match command {
                    Some('H') => {
                        let mut parts = parameters.split(';');
                        let mut next = || parts.next().and_then(|p| p.parse::<usize>().ok());
                        row = next().unwrap_or(1).saturating_sub(1);
                        column = next().unwrap_or(1).saturating_sub(1);
                    }
                    Some('J') if parameters == "2" => {
                        grid = vec![vec![' '; COLUMNS]; ROWS];
                    }
                    _ => {}
                }
            }
            '\r' => column = 0,
            '\n' => row = (row + 1).min(ROWS - 1),
            _ if character.is_control() => {}
            _ => {
                if row < ROWS && column < COLUMNS {
                    grid[row][column] = character;
                }
                column += 1;
            }
        }
    }
    grid.iter()
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
