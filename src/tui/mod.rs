//! The interactive interface, for `nixblend` without a command.
//!
//! This is a presentation layer, like `cli`. It holds no business rules: the use cases run
//! in their own thread and talk to it through the `Interaction` port, so everything they
//! validate and protect works here exactly as it does from the command line.

mod app;
mod logo;
mod logo_data;
mod theme;
mod view;
mod worker;

use std::io::{self, Stdout, Write};
use std::panic;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::application::ports::{Interaction, ProgressReporter, Services, SystemRebuilder};
use app::{App, Effect};
use theme::Theme;
use worker::{
    ChannelInteraction, ChannelProgress, Job, Reply, SuspendingRebuilder, WorkerEvent, run_jobs,
};

/// How often the screen is refreshed without any event.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Exit code Rust uses for a panic.
const PANICKED: i32 = 101;

/// Connects the adapters and hands the result to the given function.
pub type Compose = Box<
    dyn FnOnce(
            &dyn Interaction,
            &dyn ProgressReporter,
            &dyn SystemRebuilder,
            &mut dyn FnMut(Services),
        ) + Send,
>;

/// What the interface needs from the rest of the program.
pub struct Backend {
    /// Discreet identification of the system, for the header.
    pub context: String,
    pub rebuilder: Box<dyn SystemRebuilder + Send>,
    pub compose: Compose,
    /// Ends what is running, so that the use case gets to fail and finish.
    pub cancel: fn(),
    /// Leaves nothing behind, for when the process is about to end.
    pub cleanup: fn(),
}

/// How the interface ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Quit,
    Interrupted,
}

/// Gives the terminal back, in its normal state. Failures are ignored because this also runs
/// when things already went wrong, and there is nothing better to do with them.
pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Restores the terminal however the scope ends.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn enter_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(io::stdout()))
}

/// Lets a panic message be read: without this, it would be lost with the alternate screen.
fn restore_on_panic(cleanup: fn()) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
        cleanup();
        // A panic in the thread of the use cases would leave the interface waiting forever.
        process::exit(PANICKED);
    }));
}

/// Runs the interface until the user leaves.
pub fn run(backend: Backend) -> io::Result<Outcome> {
    let (job_sender, jobs) = mpsc::channel::<Job>();
    let (event_sender, events) = mpsc::channel::<WorkerEvent>();
    let (reply_sender, replies) = mpsc::channel::<Reply>();
    let (terminal_free_sender, terminal_free) = mpsc::channel::<Reply>();
    let first_valid_search = Arc::new(AtomicU64::new(0));

    let Backend {
        context,
        rebuilder,
        compose,
        cancel,
        cleanup,
    } = backend;

    restore_on_panic(cleanup);
    let worker_validity = Arc::clone(&first_valid_search);
    thread::Builder::new()
        .name("use-cases".to_owned())
        .spawn(move || {
            let interaction = ChannelInteraction {
                events: event_sender.clone(),
                replies,
            };
            let progress = ChannelProgress {
                events: event_sender.clone(),
            };
            let suspending = SuspendingRebuilder {
                inner: rebuilder.as_ref(),
                events: event_sender.clone(),
                terminal_free,
            };
            compose(&interaction, &progress, &suspending, &mut |services| {
                run_jobs(services, &jobs, &event_sender, &worker_validity);
            });
        })?;

    let _guard = TerminalGuard;
    let mut terminal = enter_terminal()?;
    let theme = Theme::from_environment();

    let loop_state = LoopState {
        jobs: job_sender,
        events,
        replies: reply_sender,
        terminal_free: terminal_free_sender,
        first_valid_search,
        cancel,
    };
    let outcome = event_loop(&mut terminal, &theme, &context, &loop_state);
    // Whatever is still running will not get the chance to finish on its own.
    cleanup();
    outcome
}

/// The channels to the use cases.
struct LoopState {
    jobs: Sender<Job>,
    events: Receiver<WorkerEvent>,
    replies: Sender<Reply>,
    terminal_free: Sender<Reply>,
    first_valid_search: Arc<AtomicU64>,
    cancel: fn(),
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    theme: &Theme,
    context: &str,
    state: &LoopState,
) -> io::Result<Outcome> {
    let (mut app, effects) = App::new(Instant::now());
    if let Some(outcome) = apply(effects, state) {
        return Ok(outcome);
    }

    loop {
        app.wide = view::is_wide(terminal.size()?.width);
        terminal.draw(|frame| view::draw(frame, &app, theme, context))?;

        if event::poll(POLL_INTERVAL)? {
            // A resize needs nothing but the next drawing, which handles any size.
            if let Event::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
            {
                let effects = app.on_key(key, Instant::now());
                if let Some(outcome) = apply(effects, state) {
                    return Ok(outcome);
                }
            }
        }

        while let Ok(worker_event) = state.events.try_recv() {
            if worker_event == WorkerEvent::Suspend {
                lend_terminal(terminal, state)?;
                continue;
            }
            let effects = app.on_worker(worker_event, Instant::now());
            if let Some(outcome) = apply(effects, state) {
                return Ok(outcome);
            }
        }

        let effects = app.on_tick(Instant::now());
        if let Some(outcome) = apply(effects, state) {
            return Ok(outcome);
        }
    }
}

/// Carries out what the state asked for. Returns how the interface ended, if it did.
fn apply(effects: Vec<Effect>, state: &LoopState) -> Option<Outcome> {
    for effect in effects {
        match effect {
            Effect::Submit(job) => {
                let _ = state.jobs.send(job);
            }
            Effect::Reply(reply) => {
                let _ = state.replies.send(reply);
            }
            Effect::Cancel { first_valid_search } => {
                state
                    .first_valid_search
                    .fetch_max(first_valid_search, Ordering::SeqCst);
                (state.cancel)();
            }
            Effect::Quit => return Some(Outcome::Quit),
            Effect::Interrupt => return Some(Outcome::Interrupted),
        }
    }
    None
}

/// Gives the terminal to the rebuild, which shows its own output and where `sudo` asks for
/// the password, and takes it back when the rebuild ends.
fn lend_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &LoopState,
) -> io::Result<()> {
    restore_terminal();
    let mut stdout = io::stdout();
    writeln!(stdout, "Running the NixOS rebuild...")?;
    stdout.flush()?;
    let _ = state.terminal_free.send(Reply::Suspended);

    // Nothing else happens while the rebuild runs: the use case is waiting for it.
    let mut failed = false;
    while let Ok(event) = state.events.recv() {
        if let WorkerEvent::Resume { failed: result } = event {
            failed = result;
            break;
        }
    }
    if failed {
        // What the rebuild printed would vanish with the switch back to the interface.
        writeln!(stdout, "\nThe rebuild failed. Press Enter to go back.")?;
        stdout.flush()?;
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }

    // A new terminal forgets what it drew. `clear` would ask the terminal where the cursor
    // is, and not every terminal answers.
    *terminal = enter_terminal()?;
    execute!(io::stdout(), Clear(ClearType::All))
}
