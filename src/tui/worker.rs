//! Runs the use cases away from the drawing loop.
//!
//! The use cases are synchronous and talk to the user through [`Interaction`]. Here, that
//! port sends what they want to say to the interface and blocks until it answers, so they
//! run exactly as they do from the command line, and the screen never freezes.

use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use crate::application::error::{AppError, ConfigError, RebuildError};
use crate::application::get_package_info::{GetPackageInfo, PackageInfo};
use crate::application::install_package::InstallPackage;
use crate::application::list_packages::{ListPackages, PackageList};
use crate::application::ports::{
    Choice, Interaction, Notice, ProgressReporter, Question, Services, SystemRebuilder,
};
use crate::application::reinstall_package::ReinstallPackage;
use crate::application::remove_package::RemovePackage;
use crate::application::search_packages::{SearchPackages, SearchResults};
use crate::domain::package::Package;

/// Work for the use cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    LoadList,
    Search(String),
    Details(String),
    Install(String),
    Remove(String),
    Reinstall(String),
}

/// A job, with its place in the order of submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
}

/// Why there is no list of declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListFailure {
    NotConfigured,
    Other(String),
}

/// What a finished job produced. Errors are their full messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult {
    List(Result<PackageList, ListFailure>),
    Search {
        id: u64,
        result: Result<Option<SearchResults>, String>,
    },
    /// The search was superseded before it started.
    SearchSkipped,
    Details {
        name: String,
        result: Result<Box<PackageInfo>, String>,
    },
    Operation(Result<(), String>),
}

/// What the use cases have to say to the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    Notice(Notice),
    Confirm(Question),
    AskPath,
    Choose {
        requested: String,
        page: Vec<Package>,
        first_index: usize,
        has_more: bool,
        offer_search: bool,
    },
    Progress {
        label: String,
        seconds: u64,
    },
    ProgressDone,
    /// The terminal is needed by the rebuild, for its output and for `sudo`.
    Suspend,
    /// The rebuild ended.
    Resume {
        failed: bool,
    },
    Finished(JobResult),
}

/// The interface's answer to a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Confirm(bool),
    Path(Option<String>),
    Choice(Choice),
    /// The terminal is free for the rebuild.
    Suspended,
}

fn gone() -> io::Error {
    io::Error::new(ErrorKind::BrokenPipe, "the interface is gone")
}

/// [`Interaction`] through the channels.
pub struct ChannelInteraction {
    pub events: Sender<WorkerEvent>,
    pub replies: Receiver<Reply>,
}

impl ChannelInteraction {
    fn send(&self, event: WorkerEvent) -> io::Result<()> {
        self.events.send(event).map_err(|_| gone())
    }

    fn receive(&self) -> io::Result<Reply> {
        self.replies.recv().map_err(|_| gone())
    }
}

impl Interaction for ChannelInteraction {
    fn notify(&self, notice: &Notice) -> io::Result<()> {
        self.send(WorkerEvent::Notice(notice.clone()))
    }

    fn confirm(&self, question: &Question) -> io::Result<bool> {
        self.send(WorkerEvent::Confirm(question.clone()))?;
        match self.receive()? {
            Reply::Confirm(answer) => Ok(answer),
            _ => Ok(false),
        }
    }

    fn ask_packages_path(&self) -> io::Result<Option<String>> {
        self.send(WorkerEvent::AskPath)?;
        match self.receive()? {
            Reply::Path(path) => Ok(path),
            _ => Ok(None),
        }
    }

    fn choose_package(
        &self,
        requested: &str,
        page: &[Package],
        first_index: usize,
        has_more: bool,
        offer_search: bool,
    ) -> io::Result<Choice> {
        self.send(WorkerEvent::Choose {
            requested: requested.to_owned(),
            page: page.to_vec(),
            first_index,
            has_more,
            offer_search,
        })?;
        match self.receive()? {
            Reply::Choice(choice) => Ok(choice),
            _ => Ok(Choice::Cancel),
        }
    }
}

/// [`ProgressReporter`] through the channel.
pub struct ChannelProgress {
    pub events: Sender<WorkerEvent>,
}

impl ProgressReporter for ChannelProgress {
    fn update(&self, label: &str, seconds: u64) {
        let _ = self.events.send(WorkerEvent::Progress {
            label: label.to_owned(),
            seconds,
        });
    }

    fn finish(&self) {
        let _ = self.events.send(WorkerEvent::ProgressDone);
    }
}

/// Lends the terminal to the rebuild, whose output and `sudo` prompt must reach the user.
pub struct SuspendingRebuilder<'a> {
    pub inner: &'a dyn SystemRebuilder,
    pub events: Sender<WorkerEvent>,
    pub terminal_free: Receiver<Reply>,
}

impl SystemRebuilder for SuspendingRebuilder<'_> {
    fn rebuild(&self) -> Result<(), RebuildError> {
        let suspended = self.events.send(WorkerEvent::Suspend).is_ok()
            && matches!(self.terminal_free.recv(), Ok(Reply::Suspended));
        if !suspended {
            return Err(RebuildError::UntrustedTool(
                "the interface could not free the terminal".to_owned(),
            ));
        }
        let result = self.inner.rebuild();
        let _ = self.events.send(WorkerEvent::Resume {
            failed: result.is_err(),
        });
        result
    }
}

/// Runs jobs until the interface goes away. Searches older than `first_valid_search` were
/// superseded while they waited, and are not worth starting.
pub fn run_jobs(
    services: Services,
    jobs: &Receiver<Job>,
    events: &Sender<WorkerEvent>,
    first_valid_search: &Arc<AtomicU64>,
) {
    while let Ok(job) = jobs.recv() {
        let result = match job.kind {
            JobKind::Search(_) if job.id < first_valid_search.load(Ordering::SeqCst) => {
                JobResult::SearchSkipped
            }
            kind => execute(services, job.id, kind),
        };
        if events.send(WorkerEvent::Finished(result)).is_err() {
            return;
        }
    }
}

/// Runs one use case. This is the only place where the interface reaches them.
fn execute(services: Services, id: u64, kind: JobKind) -> JobResult {
    match kind {
        JobKind::LoadList => JobResult::List(ListPackages { services }.execute(false).map_err(
            |error| match error {
                AppError::Config(ConfigError::NotConfigured) => ListFailure::NotConfigured,
                other => ListFailure::Other(other.to_string()),
            },
        )),
        JobKind::Search(term) => {
            let result = SearchPackages { services }
                .execute(&term)
                .map_err(|error| error.to_string());
            JobResult::Search { id, result }
        }
        JobKind::Details(name) => {
            let result = GetPackageInfo { services }
                .execute(&name)
                .map(Box::new)
                .map_err(|error| error.to_string());
            JobResult::Details { name, result }
        }
        JobKind::Install(name) => operation(InstallPackage { services }.execute(&name, false)),
        JobKind::Remove(name) => operation(RemovePackage { services }.execute(&name, false)),
        JobKind::Reinstall(name) => operation(ReinstallPackage { services }.execute(&name)),
    }
}

fn operation(result: Result<(), AppError>) -> JobResult {
    JobResult::Operation(result.map_err(|error| error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::test_support::Fixture;
    use std::sync::mpsc;

    const LIST: &str = "{ pkgs, ... }: with pkgs; [ git ]";

    fn run(fixture: &Fixture, kind: JobKind) -> JobResult {
        execute(fixture.services(), 1, kind)
    }

    #[test]
    fn jobs_reach_the_use_cases() {
        let fixture = Fixture::new(LIST).with_package("foo");
        let JobResult::List(Ok(list)) = run(&fixture, JobKind::LoadList) else {
            panic!("no list");
        };
        assert_eq!(list.packages[0].name, "git");

        let JobResult::Details { result, .. } = run(&fixture, JobKind::Details("foo".to_owned()))
        else {
            panic!("no details");
        };
        assert_eq!(result.unwrap().package.identity.attr, "foo");

        let JobResult::Search { result, .. } = run(&fixture, JobKind::Search("foo".to_owned()))
        else {
            panic!("no search");
        };
        assert_eq!(result.unwrap().unwrap().packages.len(), 1);
    }

    #[test]
    fn a_missing_configuration_is_told_apart() {
        let fixture = Fixture::new(LIST).without_config();
        assert_eq!(
            run(&fixture, JobKind::LoadList),
            JobResult::List(Err(ListFailure::NotConfigured))
        );
    }

    #[test]
    fn operations_keep_the_real_error() {
        let fixture = Fixture::new(LIST).answering(&[true]);
        let JobResult::Operation(Err(message)) = run(&fixture, JobKind::Install("nope".to_owned()))
        else {
            panic!("expected a failure");
        };
        assert!(message.contains("was not found"), "{message}");
    }

    #[test]
    fn edits_go_through_the_use_cases_rules() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .answering(&[true, false]);
        assert_eq!(
            run(&fixture, JobKind::Install("foo".to_owned())),
            JobResult::Operation(Ok(()))
        );
        assert!(fixture.content().contains("foo"));
        assert!(fixture.backed_up());
    }

    #[test]
    fn superseded_searches_do_not_start() {
        let fixture = Fixture::new(LIST).with_package("foo");
        let (job_sender, jobs) = mpsc::channel();
        let (events, event_receiver) = mpsc::channel();
        job_sender
            .send(Job {
                id: 1,
                kind: JobKind::Search("foo".to_owned()),
            })
            .unwrap();
        job_sender
            .send(Job {
                id: 2,
                kind: JobKind::LoadList,
            })
            .unwrap();
        drop(job_sender);

        run_jobs(
            fixture.services(),
            &jobs,
            &events,
            &Arc::new(AtomicU64::new(2)),
        );

        assert_eq!(
            event_receiver.recv().unwrap(),
            WorkerEvent::Finished(JobResult::SearchSkipped)
        );
        assert_eq!(fixture.searches(), 0);
    }

    #[test]
    fn interaction_blocks_for_the_answer() {
        let (events, event_receiver) = mpsc::channel();
        let (reply_sender, replies) = mpsc::channel();
        let interaction = ChannelInteraction { events, replies };

        reply_sender.send(Reply::Confirm(true)).unwrap();
        assert!(interaction.confirm(&Question::ApplyEdit).unwrap());
        assert_eq!(
            event_receiver.recv().unwrap(),
            WorkerEvent::Confirm(Question::ApplyEdit)
        );

        reply_sender.send(Reply::Choice(Choice::More)).unwrap();
        assert_eq!(
            interaction
                .choose_package("x", &[], 0, true, false)
                .unwrap(),
            Choice::More
        );
    }

    #[test]
    fn a_closed_interface_is_an_error_and_never_a_yes() {
        let (events, event_receiver) = mpsc::channel();
        let (reply_sender, replies) = mpsc::channel::<Reply>();
        let interaction = ChannelInteraction { events, replies };
        drop(reply_sender);
        assert!(interaction.confirm(&Question::ApplyEdit).is_err());
        drop(event_receiver);
        assert!(interaction.notify(&Notice::NothingChanged).is_err());
    }

    #[test]
    fn the_rebuild_waits_for_the_terminal() {
        let fixture = Fixture::new(LIST);
        let (events, event_receiver) = mpsc::channel();
        let (reply_sender, terminal_free) = mpsc::channel();
        reply_sender.send(Reply::Suspended).unwrap();
        let rebuilder = SuspendingRebuilder {
            inner: fixture.services().rebuilder,
            events,
            terminal_free,
        };

        rebuilder.rebuild().unwrap();

        assert_eq!(fixture.rebuilds(), 1);
        assert_eq!(event_receiver.recv().unwrap(), WorkerEvent::Suspend);
        assert_eq!(
            event_receiver.recv().unwrap(),
            WorkerEvent::Resume { failed: false }
        );
    }

    #[test]
    fn no_rebuild_runs_without_the_terminal() {
        let fixture = Fixture::new(LIST);
        let (events, _event_receiver) = mpsc::channel();
        let (reply_sender, terminal_free) = mpsc::channel::<Reply>();
        drop(reply_sender);
        let rebuilder = SuspendingRebuilder {
            inner: fixture.services().rebuilder,
            events,
            terminal_free,
        };
        assert!(rebuilder.rebuild().is_err());
        assert_eq!(fixture.rebuilds(), 0);
    }
}
