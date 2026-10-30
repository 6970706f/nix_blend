//! Runs external tools with a time limit, so that the terminal is never blocked
//! indefinitely.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to wait for the output after the process ended. A descendant holding the
/// pipes open must not keep the caller blocked.
const DRAIN_LIMIT: Duration = Duration::from_secs(2);

/// Why a process did not produce an output.
#[derive(Debug)]
pub enum ProcessError {
    NotFound,
    Spawn(io::Error),
    TimedOut(Duration),
}

/// Runs `command`, capturing its output, and kills it after `timeout`. `tick` receives
/// the elapsed seconds about once a second. Standard input is left to the caller.
pub fn run(
    command: &mut Command,
    timeout: Duration,
    tick: &mut dyn FnMut(u64),
) -> Result<Output, ProcessError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ProcessError::NotFound,
            _ => ProcessError::Spawn(error),
        })?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let started = Instant::now();
    let mut reported = 0;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(ProcessError::Spawn)? {
            break status;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let _ = child.kill();
            let _ = child.wait();
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
