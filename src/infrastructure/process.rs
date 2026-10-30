//! Runs external tools with a time limit, so that the terminal is never blocked
//! indefinitely.

use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

const SIGKILL: i32 = 9;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to wait for the output after the process ended. A descendant holding the
/// pipes open must not keep the caller blocked.
const DRAIN_LIMIT: Duration = Duration::from_secs(2);

/// Process groups of the tools running right now, for cleanup when interrupted.
static LIVE_GROUPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Why a process did not produce an output.
#[derive(Debug)]
pub enum ProcessError {
    NotFound,
    Spawn(io::Error),
    TimedOut(Duration),
}

/// Runs `command`, capturing its output, and kills it after `timeout`. `tick` receives
/// the elapsed seconds about once a second. Standard input is left to the caller.
///
/// The tool gets its own process group, so that what it started goes with it. Ctrl+C then
/// does not reach it directly: the signal handler ends the group, with [`terminate_all`].
pub fn run(
    command: &mut Command,
    timeout: Duration,
    tick: &mut dyn FnMut(u64),
) -> Result<Output, ProcessError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ProcessError::NotFound,
            _ => ProcessError::Spawn(error),
        })?;
    let group = child.id();
    track(group);
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let started = Instant::now();
    let mut reported = 0;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(ProcessError::Spawn)? {
            untrack(group);
            break status;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            signal_group(group, Signal::KILL);
            let _ = child.wait();
            untrack(group);
            return Err(ProcessError::TimedOut(timeout));
        }
        if elapsed.as_secs() > reported {
            reported = elapsed.as_secs();
            tick(reported);
        }
        thread::sleep(POLL_INTERVAL);
    };
    Ok(Output {
        status,
        stdout: stdout.recv_timeout(DRAIN_LIMIT).unwrap_or_default(),
        stderr: stderr.recv_timeout(DRAIN_LIMIT).unwrap_or_default(),
    })
}

fn track(group: u32) {
    if let Ok(mut groups) = LIVE_GROUPS.lock() {
        groups.push(group);
    }
}

fn untrack(group: u32) {
    if let Ok(mut groups) = LIVE_GROUPS.lock() {
        groups.retain(|live| *live != group);
    }
}

fn signal_group(group: u32, signal: Signal) {
    if let Some(pid) = i32::try_from(group).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, signal);
    }
}

/// Asks the tools running right now to end. Used when interrupted.
pub fn terminate_all() {
    if let Ok(groups) = LIVE_GROUPS.lock() {
        for group in groups.iter() {
            signal_group(*group, Signal::TERM);
        }
    }
}

/// What to say about a tool that failed: its error from the first `error:` on, without
/// progress lines. A tool can also die without a word, as when it runs out of memory.
pub fn failure_reason(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let start = stderr.find("error:").unwrap_or(0);
    let message = stderr[start..].trim();
    if !message.is_empty() {
        return message.to_owned();
    }
    if output.status.signal() == Some(SIGKILL) {
        return format!(
            "the tool was killed ({}). The system may have run out of memory",
            output.status
        );
    }
    format!("the tool ended with {} and reported nothing", output.status)
}

/// Reads a pipe in the background, so that a full pipe cannot stall the process.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut content = Vec::new();
            let _ = pipe.read_to_end(&mut content);
            let _ = sender.send(content);
        });
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_quietly(command: &mut Command, timeout: Duration) -> Result<Output, ProcessError> {
        run(command.stdin(Stdio::null()), timeout, &mut |_| {})
    }

    #[test]
    fn captures_output_and_status() {
        let output = run_quietly(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn kills_the_process_at_the_time_limit() {
        let started = Instant::now();
        let result = run_quietly(Command::new("sleep").arg("30"), Duration::from_millis(200));
        assert!(matches!(result, Err(ProcessError::TimedOut(_))));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_time_limit_also_ends_what_the_tool_started() {
        let started = Instant::now();
        let result = run_quietly(
            Command::new("sh").args(["-c", "sleep 31 & wait"]),
            Duration::from_millis(200),
        );
        assert!(matches!(result, Err(ProcessError::TimedOut(_))));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    fn output_of(script: &str) -> Output {
        run_quietly(
            Command::new("sh").args(["-c", script]),
            Duration::from_secs(10),
        )
        .unwrap()
    }

    #[test]
    fn failure_reason_explains_silent_deaths() {
        assert!(failure_reason(&output_of("kill -9 $$")).contains("run out of memory"));
        assert!(failure_reason(&output_of("exit 4")).contains("reported nothing"));
        assert_eq!(
            failure_reason(&output_of("echo progress >&2; echo error: bad >&2; exit 1")),
            "error: bad"
        );
        assert_eq!(
            failure_reason(&output_of("echo other >&2; exit 1")),
            "other"
        );
    }

    #[test]
    fn reports_a_missing_program() {
        let result = run_quietly(
            &mut Command::new("/nonexistent/tool"),
            Duration::from_secs(1),
        );
        assert!(matches!(result, Err(ProcessError::NotFound)));
    }

    #[test]
    fn handles_more_output_than_a_pipe_holds() {
        let output = run_quietly(
            Command::new("sh").args(["-c", "yes | head -c 300000"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(output.stdout.len(), 300_000);
    }

    #[test]
    fn reports_progress() {
        let mut ticks = Vec::new();
        run(
            Command::new("sleep").arg("1.2").stdin(Stdio::null()),
            Duration::from_secs(10),
            &mut |seconds| ticks.push(seconds),
        )
        .unwrap();
        assert_eq!(ticks, [1]);
    }
}
