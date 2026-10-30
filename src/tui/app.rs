//! State of the interface and what each key does. Nothing here draws or runs anything: it
//! takes events and returns effects, so all of it can be tested without a terminal.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::worker::{Job, JobKind, JobResult, ListFailure, Reply, WorkerEvent};
use crate::application::get_package_info::PackageInfo;
use crate::application::list_packages::{ListedPackage, PackageList};
use crate::application::ports::{Choice, Notice, Question};
use crate::cli::presenter;
use crate::domain::package::{Package, PackageSource};

/// How long the selection must rest before its details are fetched.
const DETAILS_DELAY: Duration = Duration::from_millis(350);
const PAGE: usize = 10;

/// What the interface wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Submit(Job),
    Reply(Reply),
    /// End what is running, and do not start searches older than this job id.
    Cancel {
        first_valid_search: u64,
    },
    Quit,
    Interrupt,
}

/// What the list shows when it is not showing search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    Loading,
    Loaded(PackageList),
    NotConfigured,
    Failed(String),
}

/// State of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchState {
    Running,
    Done(Vec<Package>),
    /// The search did not run, as the user preferred, given a likely typo.
    NotRun,
    Failed(String),
}

/// A search and its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub term: String,
    pub state: SearchState,
    pub job: u64,
}

/// What is known about the package in the details panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Details {
    Loading,
    Loaded(Box<PackageInfo>),
    Failed(String),
}

/// A choice among packages, inside an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choosing {
    pub requested: String,
    pub options: Vec<Package>,
    pub highlighted: usize,
    pub has_more: bool,
    pub offer_search: bool,
}

/// Where an operation is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Running,
    Confirming(Question),
    Choosing(Choosing),
    AskingPath(String),
    Succeeded,
    Failed(String),
}

/// An install, a removal or a reinstallation, from start to finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub verb: &'static str,
    pub package: String,
    pub source: String,
    pub file: Option<PathBuf>,
    pub log: Vec<String>,
    pub diff: Option<String>,
    pub scroll: u16,
    pub phase: Phase,
    pub cancelling: bool,
}

/// A reinstallation only needs one step here: there is nothing to show in a diff, and it
/// rebuilds the system right away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReinstall {
    pub package: String,
    pub source: String,
}

/// What occupies the main area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Browse,
    Details,
    Help,
    ConfirmReinstall(PendingReinstall),
    Operation(Box<Operation>),
}

/// What the selected row is.
#[derive(Debug, Clone, Copy)]
pub enum Selected<'a> {
    Declared(&'a ListedPackage),
    Result(&'a Package),
}

/// How a search result relates to the declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredMark {
    No,
    Yes,
    /// The same name, from this other source.
    Elsewhere(String),
}

/// One action on the key bar.
pub type Hint = (&'static str, &'static str);

/// The whole state of the interface.
#[derive(Debug)]
pub struct App {
    pub screen: Screen,
    pub editing: bool,
    pub input: String,
    pub declared: Declared,
    pub search: Option<Search>,
    pub selected: usize,
    pub progress: Option<(String, u64)>,
    pub wide: bool,
    details: HashMap<String, Details>,
    moved_at: Instant,
    running: VecDeque<u64>,
    next_job: u64,
    /// The search job in flight, if any.
    search_job: Option<u64>,
}

impl App {
    /// The state at startup, with what must run first.
    pub fn new(now: Instant) -> (Self, Vec<Effect>) {
        let mut app = Self {
            screen: Screen::Browse,
            editing: false,
            input: String::new(),
            declared: Declared::Loading,
            search: None,
            selected: 0,
            progress: None,
            wide: true,
            details: HashMap::new(),
            moved_at: now,
            running: VecDeque::new(),
            next_job: 1,
            search_job: None,
        };
        let effect = app.submit(JobKind::LoadList);
        (app, vec![effect])
    }

    fn submit(&mut self, kind: JobKind) -> Effect {
        let id = self.next_job;
        self.next_job += 1;
        self.running.push_back(id);
        Effect::Submit(Job { id, kind })
    }

    /// Rows of the list being shown.
    pub fn row_count(&self) -> usize {
        match &self.search {
            Some(Search {
                state: SearchState::Done(packages),
                ..
            }) => packages.len(),
            Some(_) => 0,
            None => match &self.declared {
                Declared::Loaded(list) => list.packages.len(),
                _ => 0,
            },
        }
    }

    /// The selected row, if the list has any.
    pub fn selected_row(&self) -> Option<Selected<'_>> {
        match &self.search {
            Some(Search {
                state: SearchState::Done(packages),
                ..
            }) => packages.get(self.selected).map(Selected::Result),
            Some(_) => None,
            None => match &self.declared {
                Declared::Loaded(list) => list.packages.get(self.selected).map(Selected::Declared),
                _ => None,
            },
        }
    }

    /// The declaration with the same identity as `package`.
    fn declaration_of(&self, package: &Package) -> Option<&ListedPackage> {
        let Declared::Loaded(list) = &self.declared else {
            return None;
        };
        list.packages
            .iter()
            .find(|listed| listed.identity.as_ref() == Some(&package.identity))
    }

    /// How a search result relates to the declarations. This is only a mark on the screen:
    /// the use cases decide for themselves when they are asked to act.
    pub fn mark_of(&self, package: &Package) -> DeclaredMark {
        if self.declaration_of(package).is_some() {
            return DeclaredMark::Yes;
        }
        let Declared::Loaded(list) = &self.declared else {
            return DeclaredMark::No;
        };
        list.packages
            .iter()
            .find(|listed| {
                listed
                    .identity
                    .as_ref()
                    .is_some_and(|identity| identity.attr == package.identity.attr)
            })
            .map_or(DeclaredMark::No, |listed| {
                DeclaredMark::Elsewhere(listed.source.clone())
            })
    }

    /// The name to ask a use case about the selected row, for details.
    fn details_name(&self) -> Option<String> {
        match self.selected_row()? {
            Selected::Result(package) => Some(package.identity.qualified()),
            Selected::Declared(listed) => {
                let identity = listed.identity.as_ref()?;
                // These cannot be looked up in a package catalog.
                match identity.source {
                    PackageSource::FlakeInput(_) | PackageSource::Unknown => None,
                    _ => listed.request_name(),
                }
            }
        }
    }

    /// Details of the selected row, if any were fetched.
    pub fn selected_details(&self) -> Option<&Details> {
        self.details.get(&self.details_name()?)
    }

    /// The name to remove the selected row by, if it is declared.
    fn removal_name(&self) -> Option<String> {
        match self.selected_row()? {
            Selected::Declared(listed) => listed.request_name(),
            Selected::Result(package) => self.declaration_of(package)?.request_name(),
        }
    }

    /// The selected search result, if it is not declared yet.
    fn installable(&self) -> Option<&Package> {
        match self.selected_row()? {
            Selected::Result(package) if self.declaration_of(package).is_none() => Some(package),
            _ => None,
        }
    }

    /// The actions that can be used right now.
    pub fn hints(&self) -> Vec<Hint> {
        match &self.screen {
            Screen::Help => vec![("Esc", "Back")],
            Screen::Details => vec![("Esc", "Back"), ("q", "Quit")],
            Screen::ConfirmReinstall(_) => vec![("y", "Rebuild"), ("n", "Cancel")],
            Screen::Operation(operation) => operation_hints(operation),
            Screen::Browse if self.editing => {
                vec![("Enter", "Search"), ("Down", "Results"), ("Esc", "Back")]
            }
            Screen::Browse => self.browse_hints(),
        }
    }

    fn browse_hints(&self) -> Vec<Hint> {
        let mut hints = vec![("/", "Search")];
        if self.installable().is_some() {
            hints.push(("i", "Install"));
        }
        if self.removal_name().is_some() {
            hints.push(("r", "Remove"));
            hints.push(("R", "Reinstall"));
        }
        if self.selected_row().is_some() {
            hints.push(("Enter", "Details"));
        }
        if self.search.is_some() {
            hints.push(("Esc", "Declared"));
        }
        hints.push(("?", "Keys"));
        hints.push(("q", "Quit"));
        hints
    }

    /// Reacts to a key.
    pub fn on_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return vec![Effect::Interrupt];
        }
        match &self.screen {
            Screen::Help => {
                self.screen = Screen::Browse;
                Vec::new()
            }
            Screen::Details => self.on_details_key(key),
            Screen::ConfirmReinstall(_) => self.on_confirm_reinstall_key(key),
            Screen::Operation(_) => self.on_operation_key(key),
            Screen::Browse if self.editing => self.on_input_key(key, now),
            Screen::Browse => self.on_browse_key(key, now),
        }
    }

    fn on_details_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Esc | KeyCode::Enter => {
                self.screen = Screen::Browse;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_confirm_reinstall_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Screen::ConfirmReinstall(pending) = &self.screen else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                let pending = pending.clone();
                self.start_operation(
                    "REINSTALL",
                    pending.package.clone(),
                    pending.source,
                    JobKind::Reinstall(pending.package),
                )
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.screen = Screen::Browse;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_input_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => {
                self.editing = false;
                Vec::new()
            }
            KeyCode::Down => {
                self.editing = false;
                self.move_selection(1, now);
                Vec::new()
            }
            KeyCode::Enter => self.start_search(now),
            KeyCode::Backspace => {
                self.input.pop();
                Vec::new()
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                Vec::new()
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.push(character);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn start_search(&mut self, now: Instant) -> Vec<Effect> {
        let term = self.input.trim().to_owned();
        if term.is_empty() {
            return Vec::new();
        }
        let mut effects = self.cancel_search();
        self.editing = false;
        let submit = self.submit(JobKind::Search(term.clone()));
        let Effect::Submit(job) = &submit else {
            return effects;
        };
        self.search_job = Some(job.id);
        self.search = Some(Search {
            term,
            state: SearchState::Running,
            job: job.id,
        });
        self.selected = 0;
        self.moved_at = now;
        effects.push(submit);
        effects
    }

    /// Stops a search that is no longer wanted.
    fn cancel_search(&mut self) -> Vec<Effect> {
        match self.search_job.take() {
            Some(job) => vec![Effect::Cancel {
                first_valid_search: job + 1,
            }],
            None => Vec::new(),
        }
    }

    fn on_browse_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('?') => {
                self.screen = Screen::Help;
                Vec::new()
            }
            KeyCode::Char('/') => {
                self.editing = true;
                Vec::new()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1, now);
                Vec::new()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1, now);
                Vec::new()
            }
            KeyCode::PageUp => {
                self.move_selection(-(PAGE as isize), now);
                Vec::new()
            }
            KeyCode::PageDown => {
                self.move_selection(PAGE as isize, now);
                Vec::new()
            }
            KeyCode::Home => {
                self.move_selection(isize::MIN / 2, now);
                Vec::new()
            }
            KeyCode::End => {
                self.move_selection(isize::MAX / 2, now);
                Vec::new()
            }
            KeyCode::Esc => {
                let effects = self.cancel_search();
                if self.search.take().is_some() {
                    self.selected = 0;
                    self.moved_at = now;
                }
                effects
            }
            KeyCode::Enter => self.open_details(),
            KeyCode::Char('i') => self.start_install(),
            KeyCode::Char('r') => self.start_removal(),
            KeyCode::Char('R') => self.ask_reinstall(),
            _ => Vec::new(),
        }
    }

    fn move_selection(&mut self, by: isize, now: Instant) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let last = count - 1;
        let target = self.selected.saturating_add_signed(by).min(last);
        if target != self.selected {
            self.selected = target;
            self.moved_at = now;
        }
    }

    /// Asks for the details at once, and shows them on their own where there is no panel.
    fn open_details(&mut self) -> Vec<Effect> {
        if self.selected_row().is_none() {
            return Vec::new();
        }
        let effects = self.fetch_details();
        if !self.wide {
            self.screen = Screen::Details;
        }
        effects
    }

    fn fetch_details(&mut self) -> Vec<Effect> {
        let Some(name) = self.details_name() else {
            return Vec::new();
        };
        if self.details.contains_key(&name) {
            return Vec::new();
        }
        self.details.insert(name.clone(), Details::Loading);
        vec![self.submit(JobKind::Details(name))]
    }

    fn start_install(&mut self) -> Vec<Effect> {
        let Some(package) = self.installable() else {
            return Vec::new();
        };
        let name = package.identity.qualified();
        let source = package.identity.source.label().to_owned();
        self.start_operation("INSTALL", name.clone(), source, JobKind::Install(name))
    }

    fn start_removal(&mut self) -> Vec<Effect> {
        let Some(name) = self.removal_name() else {
            return Vec::new();
        };
        let source = self.selected_source();
        self.start_operation("REMOVE", name.clone(), source, JobKind::Remove(name))
    }

    fn ask_reinstall(&mut self) -> Vec<Effect> {
        let Some(package) = self.removal_name() else {
            return Vec::new();
        };
        let source = self.selected_source();
        self.screen = Screen::ConfirmReinstall(PendingReinstall { package, source });
        Vec::new()
    }

    fn selected_source(&self) -> String {
        match self.selected_row() {
            Some(Selected::Declared(listed)) => listed.source.clone(),
            Some(Selected::Result(package)) => package.identity.source.label().to_owned(),
            None => String::new(),
        }
    }

    fn start_operation(
        &mut self,
        verb: &'static str,
        package: String,
        source: String,
        kind: JobKind,
    ) -> Vec<Effect> {
        self.screen = Screen::Operation(Box::new(Operation {
            verb,
            package,
            source,
            file: None,
            log: Vec::new(),
            diff: None,
            scroll: 0,
            phase: Phase::Running,
            cancelling: false,
        }));
        vec![self.submit(kind)]
    }

    fn on_operation_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Screen::Operation(operation) = &mut self.screen else {
            return Vec::new();
        };
        match key.code {
            KeyCode::PageUp => {
                operation.scroll = operation.scroll.saturating_sub(PAGE as u16);
                return Vec::new();
            }
            KeyCode::PageDown => {
                operation.scroll = operation.scroll.saturating_add(PAGE as u16);
                return Vec::new();
            }
            _ => {}
        }
        match &mut operation.phase {
            Phase::Running => {
                if key.code == KeyCode::Esc && !operation.cancelling {
                    operation.cancelling = true;
                    operation.log.push("Cancelling...".to_owned());
                    return vec![Effect::Cancel {
                        first_valid_search: 0,
                    }];
                }
                Vec::new()
            }
            Phase::Confirming(_) => {
                let answer = match key.code {
                    KeyCode::Char('y' | 'Y') => true,
                    KeyCode::Char('n' | 'N') | KeyCode::Esc => false,
                    KeyCode::Up | KeyCode::Char('k') => {
                        operation.scroll = operation.scroll.saturating_sub(1);
                        return Vec::new();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        operation.scroll = operation.scroll.saturating_add(1);
                        return Vec::new();
                    }
                    _ => return Vec::new(),
                };
                operation.phase = Phase::Running;
                vec![Effect::Reply(Reply::Confirm(answer))]
            }
            Phase::Choosing(choosing) => {
                let choice = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        choosing.highlighted = choosing.highlighted.saturating_sub(1);
                        return Vec::new();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let last = choosing.options.len().saturating_sub(1);
                        choosing.highlighted = (choosing.highlighted + 1).min(last);
                        return Vec::new();
                    }
                    KeyCode::Enter => Choice::Index(choosing.highlighted),
                    KeyCode::Char('m') if choosing.has_more => Choice::More,
                    KeyCode::Char('s') if choosing.offer_search => Choice::SearchAll,
                    KeyCode::Esc => Choice::Cancel,
                    _ => return Vec::new(),
                };
                // The options stay until the next page is added to them.
                if choice != Choice::More {
                    operation.phase = Phase::Running;
                }
                vec![Effect::Reply(Reply::Choice(choice))]
            }
            Phase::AskingPath(path) => match key.code {
                KeyCode::Enter => {
                    let answer = path.clone();
                    operation.phase = Phase::Running;
                    vec![Effect::Reply(Reply::Path(Some(answer)))]
                }
                KeyCode::Esc => {
                    operation.phase = Phase::Running;
                    vec![Effect::Reply(Reply::Path(None))]
                }
                KeyCode::Backspace => {
                    path.pop();
                    Vec::new()
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    path.push(character);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Phase::Succeeded | Phase::Failed(_) => match key.code {
                KeyCode::Enter | KeyCode::Esc => self.close_operation(),
                KeyCode::Char('q') => vec![Effect::Quit],
                KeyCode::Up | KeyCode::Char('k') => {
                    operation.scroll = operation.scroll.saturating_sub(1);
                    Vec::new()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    operation.scroll = operation.scroll.saturating_add(1);
                    Vec::new()
                }
                _ => Vec::new(),
            },
        }
    }

    /// Goes back to the list, whose content may have changed.
    fn close_operation(&mut self) -> Vec<Effect> {
        self.screen = Screen::Browse;
        self.details.clear();
        vec![self.submit(JobKind::LoadList)]
    }

    /// Reacts to the use cases.
    pub fn on_worker(&mut self, event: WorkerEvent, now: Instant) -> Vec<Effect> {
        match event {
            WorkerEvent::Notice(notice) => {
                self.on_notice(notice);
                Vec::new()
            }
            WorkerEvent::Confirm(question) => {
                self.set_phase(Phase::Confirming(question));
                Vec::new()
            }
            WorkerEvent::AskPath => {
                self.set_phase(Phase::AskingPath(String::new()));
                Vec::new()
            }
            WorkerEvent::Choose {
                requested,
                page,
                first_index,
                has_more,
                offer_search,
            } => {
                self.on_choose(requested, page, first_index, has_more, offer_search);
                Vec::new()
            }
            WorkerEvent::Progress { label, seconds } => {
                self.progress = Some((label, seconds));
                Vec::new()
            }
            WorkerEvent::ProgressDone => {
                self.progress = None;
                Vec::new()
            }
            // Handled by the loop that owns the terminal.
            WorkerEvent::Suspend | WorkerEvent::Resume { .. } => Vec::new(),
            WorkerEvent::Finished(result) => {
                self.running.pop_front();
                self.on_finished(result, now)
            }
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if let Screen::Operation(operation) = &mut self.screen {
            operation.phase = phase;
        }
    }

    fn on_choose(
        &mut self,
        requested: String,
        page: Vec<Package>,
        first_index: usize,
        has_more: bool,
        offer_search: bool,
    ) {
        let Screen::Operation(operation) = &mut self.screen else {
            return;
        };
        let (mut options, highlighted) = match &mut operation.phase {
            Phase::Choosing(previous) if first_index > 0 => {
                (std::mem::take(&mut previous.options), first_index)
            }
            _ => (Vec::new(), 0),
        };
        options.extend(page);
        operation.phase = Phase::Choosing(Choosing {
            requested,
            options,
            highlighted,
            has_more,
            offer_search,
        });
    }

    fn on_notice(&mut self, notice: Notice) {
        let Screen::Operation(operation) = &mut self.screen else {
            return;
        };
        match &notice {
            Notice::Diff { path, old, new } => {
                operation.file = Some(path.clone());
                operation.diff = Some(presenter::render_diff(path, old, new));
                return;
            }
            Notice::PackageFound(package) => {
                package
                    .identity
                    .source
                    .label()
                    .clone_into(&mut operation.source);
                operation.package = package.identity.qualified();
            }
            Notice::PackageAdded { path, .. }
            | Notice::PackageRemoved { path, .. }
            | Notice::AlreadyDeclared { path, .. } => {
                operation.file = Some(path.clone());
            }
            _ => {}
        }
        operation.log.push(presenter::notice(&notice));
    }

    fn on_finished(&mut self, result: JobResult, now: Instant) -> Vec<Effect> {
        match result {
            JobResult::List(list) => {
                self.declared = match list {
                    Ok(list) => Declared::Loaded(list),
                    Err(ListFailure::NotConfigured) => Declared::NotConfigured,
                    Err(ListFailure::Other(message)) => Declared::Failed(message),
                };
                self.selected = self.selected.min(self.row_count().saturating_sub(1));
                self.moved_at = now;
            }
            JobResult::SearchSkipped => {}
            JobResult::Search { id, result } => self.on_search_finished(id, result, now),
            JobResult::Details { name, result } => {
                let details = match result {
                    Ok(info) => Details::Loaded(info),
                    Err(message) => Details::Failed(message),
                };
                self.details.insert(name, details);
            }
            JobResult::Operation(result) => {
                if let Screen::Operation(operation) = &mut self.screen {
                    operation.phase = match result {
                        Ok(()) => Phase::Succeeded,
                        Err(message) => Phase::Failed(message),
                    };
                }
            }
        }
        Vec::new()
    }

    fn on_search_finished(
        &mut self,
        id: u64,
        result: Result<Option<crate::application::search_packages::SearchResults>, String>,
        now: Instant,
    ) {
        let Some(search) = &mut self.search else {
            return;
        };
        // An answer to an older question is not an answer to this one.
        if search.job != id {
            return;
        }
        search.state = match result {
            Ok(Some(results)) => SearchState::Done(results.packages),
            Ok(None) => SearchState::NotRun,
            Err(message) => SearchState::Failed(message),
        };
        self.search_job = None;
        self.selected = 0;
        self.moved_at = now;
    }

    /// Fetches what the selection needs, once it stopped moving and nothing else is running.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Effect> {
        let idle = self.running.is_empty() && matches!(self.screen, Screen::Browse);
        if idle && !self.editing && now.duration_since(self.moved_at) >= DETAILS_DELAY {
            return self.fetch_details();
        }
        Vec::new()
    }

    /// Whether something is running that the user should know about.
    pub fn is_searching(&self) -> bool {
        matches!(
            self.search,
            Some(Search {
                state: SearchState::Running,
                ..
            })
        )
    }
}

fn operation_hints(operation: &Operation) -> Vec<Hint> {
    match &operation.phase {
        Phase::Running if operation.cancelling => Vec::new(),
        Phase::Running => vec![("Esc", "Cancel")],
        Phase::Confirming(_) => {
            let mut hints = vec![("y", "Yes"), ("n", "No")];
            if operation.diff.is_some() {
                hints.push(("Up/Down", "Scroll"));
            }
            hints
        }
        Phase::Choosing(choosing) => {
            let mut hints = vec![("Enter", "Select")];
            if choosing.has_more {
                hints.push(("m", "More"));
            }
            if choosing.offer_search {
                hints.push(("s", "Search everything"));
            }
            hints.push(("Esc", "Cancel"));
            hints
        }
        Phase::AskingPath(_) => vec![("Enter", "Confirm"), ("Esc", "Cancel")],
        Phase::Succeeded | Phase::Failed(_) => {
            vec![("Enter", "Back"), ("Up/Down", "Scroll"), ("q", "Quit")]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::search_packages::SearchResults;
    use crate::domain::package::{PackageIdentity, PackageMetadata};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now())
    }

    fn type_text(app: &mut App, text: &str) {
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    fn listed(name: &str, source: PackageSource) -> ListedPackage {
        ListedPackage {
            name: name.to_owned(),
            source: source.label().to_owned(),
            identity: Some(PackageIdentity {
                source,
                attr: name.to_owned(),
            }),
            active: None,
        }
    }

    fn unstable() -> PackageSource {
        PackageSource::Channel {
            alias: "unstable".to_owned(),
            input: "nixpkgs-unstable".to_owned(),
        }
    }

    fn package(attr: &str) -> Package {
        Package {
            identity: PackageIdentity::nixpkgs(attr),
            metadata: PackageMetadata::default(),
        }
    }

    /// An app with the declarations already loaded.
    fn loaded() -> App {
        let (mut app, _) = App::new(Instant::now());
        app.on_worker(
            WorkerEvent::Finished(JobResult::List(Ok(PackageList {
                path: PathBuf::from("/p.nix"),
                packages: vec![
                    listed("git", PackageSource::Nixpkgs),
                    listed("vscode", unstable()),
                ],
                warning: None,
            }))),
            Instant::now(),
        );
        app
    }

    fn search_id(app: &App) -> u64 {
        app.search.as_ref().map_or(0, |search| search.job)
    }

    fn with_results(attrs: &[&str]) -> App {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Enter);
        let id = search_id(&app);
        app.on_worker(
            WorkerEvent::Finished(JobResult::Search {
                id,
                result: Ok(Some(SearchResults {
                    term: "x".to_owned(),
                    packages: attrs.iter().map(|attr| package(attr)).collect(),
                    pinned: true,
                })),
            }),
            Instant::now(),
        );
        app
    }

    fn submitted(effects: &[Effect]) -> Vec<JobKind> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Submit(job) => Some(job.kind.clone()),
                _ => None,
            })
            .collect()
    }

    fn keys(app: &App) -> Vec<&'static str> {
        app.hints().iter().map(|(key, _)| *key).collect()
    }

    #[test]
    fn startup_loads_the_declarations() {
        let (_, effects) = App::new(Instant::now());
        assert_eq!(submitted(&effects), [JobKind::LoadList]);
    }

    #[test]
    fn letters_are_text_while_typing_a_search() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        for key in ['i', 'r', 'q', 'R', '?', '/', 'j', 'k'] {
            let effects = press(&mut app, KeyCode::Char(key));
            assert!(effects.is_empty(), "{key} did something");
        }
        assert_eq!(app.input, "irqR?/jk");
        assert!(app.editing);
    }

    #[test]
    fn typing_keeps_editing_keys_working() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "ab");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input, "a");
        app.on_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            Instant::now(),
        );
        assert_eq!(app.input, "");
        press(&mut app, KeyCode::Esc);
        assert!(!app.editing);
    }

    #[test]
    fn an_empty_search_does_nothing() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "   ");
        assert!(press(&mut app, KeyCode::Enter).is_empty());
    }

    #[test]
    fn q_quits_and_ctrl_c_interrupts_anywhere() {
        let mut app = loaded();
        assert_eq!(press(&mut app, KeyCode::Char('q')), [Effect::Quit]);
        press(&mut app, KeyCode::Char('/'));
        let effects = app.on_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Instant::now(),
        );
        assert_eq!(effects, [Effect::Interrupt]);
    }

    #[test]
    fn arrows_stay_inside_the_list() {
        let mut app = loaded();
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::End);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn a_search_shows_results_and_esc_goes_back() {
        let mut app = with_results(&["foo", "bar"]);
        assert_eq!(app.row_count(), 2);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Esc);
        assert!(app.search.is_none());
        assert_eq!(app.selected, 0);
        assert_eq!(app.row_count(), 2);
    }

    #[test]
    fn results_without_matches_have_no_rows() {
        let app = with_results(&[]);
        assert_eq!(app.row_count(), 0);
        assert!(app.selected_row().is_none());
    }

    #[test]
    fn a_new_search_stops_the_previous_one() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "one");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "two");
        let effects = press(&mut app, KeyCode::Enter);
        assert!(matches!(effects[0], Effect::Cancel { .. }));
        assert_eq!(
            submitted(&effects),
            [JobKind::Search("onetwo".to_owned())],
            "the text stays in the field"
        );
    }

    #[test]
    fn an_answer_to_an_older_search_is_ignored() {
        let mut app = with_results(&["foo"]);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "y");
        press(&mut app, KeyCode::Enter);
        let stale = search_id(&app) - 1;
        app.on_worker(
            WorkerEvent::Finished(JobResult::Search {
                id: stale,
                result: Ok(Some(SearchResults {
                    term: "x".to_owned(),
                    packages: vec![package("stale")],
                    pinned: true,
                })),
            }),
            Instant::now(),
        );
        assert!(app.is_searching());
    }

    #[test]
    fn a_failed_search_keeps_the_message() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Enter);
        let id = search_id(&app);
        app.on_worker(
            WorkerEvent::Finished(JobResult::Search {
                id,
                result: Err("search error: offline".to_owned()),
            }),
            Instant::now(),
        );
        assert_eq!(
            app.search.unwrap().state,
            SearchState::Failed("search error: offline".to_owned())
        );
    }

    #[test]
    fn hints_only_offer_what_can_be_done() {
        let app = loaded();
        let browse = keys(&app);
        assert!(browse.contains(&"r") && browse.contains(&"R"));
        assert!(!browse.contains(&"i"), "a declared row cannot be installed");

        let app = with_results(&["foo"]);
        let results = keys(&app);
        assert!(results.contains(&"i"));
        assert!(!results.contains(&"r"), "foo is not declared");

        let app = with_results(&["git"]);
        let declared = keys(&app);
        assert!(declared.contains(&"r"));
        assert!(!declared.contains(&"i"), "git is declared");

        let app = with_results(&[]);
        let empty = keys(&app);
        assert!(!empty.contains(&"Enter") && !empty.contains(&"i") && !empty.contains(&"r"));

        let (app, _) = App::new(Instant::now());
        assert!(!keys(&app).contains(&"r"), "nothing is loaded yet");
    }

    #[test]
    fn keys_for_unavailable_actions_do_nothing() {
        let mut app = loaded();
        assert!(press(&mut app, KeyCode::Char('i')).is_empty());
        let mut app = with_results(&["foo"]);
        assert!(press(&mut app, KeyCode::Char('r')).is_empty());
        assert!(press(&mut app, KeyCode::Char('R')).is_empty());
        assert_eq!(app.screen, Screen::Browse);
    }

    #[test]
    fn removal_names_exactly_the_selected_declaration() {
        let mut app = loaded();
        press(&mut app, KeyCode::Down);
        let effects = press(&mut app, KeyCode::Char('r'));
        assert_eq!(
            submitted(&effects),
            [JobKind::Remove("unstable.vscode".to_owned())]
        );
        let Screen::Operation(operation) = &app.screen else {
            panic!("no operation");
        };
        assert_eq!(operation.verb, "REMOVE");
        assert_eq!(operation.source, "unstable");
    }

    #[test]
    fn install_starts_from_a_search_result() {
        let mut app = with_results(&["foo"]);
        let effects = press(&mut app, KeyCode::Char('i'));
        assert_eq!(submitted(&effects), [JobKind::Install("foo".to_owned())]);
    }

    #[test]
    fn the_same_name_from_another_source_is_marked() {
        let app = with_results(&["vscode", "git", "foo"]);
        assert_eq!(
            app.mark_of(&package("vscode")),
            DeclaredMark::Elsewhere("unstable".to_owned())
        );
        assert_eq!(app.mark_of(&package("git")), DeclaredMark::Yes);
        assert_eq!(app.mark_of(&package("foo")), DeclaredMark::No);
    }

    fn operating() -> App {
        let mut app = with_results(&["foo"]);
        press(&mut app, KeyCode::Char('i'));
        app
    }

    fn operation(app: &App) -> &Operation {
        match &app.screen {
            Screen::Operation(operation) => operation,
            other => panic!("not an operation: {other:?}"),
        }
    }

    #[test]
    fn nothing_is_answered_without_an_explicit_key() {
        let mut app = operating();
        app.on_worker(WorkerEvent::Confirm(Question::ApplyEdit), Instant::now());
        for key in [KeyCode::Enter, KeyCode::Char('x'), KeyCode::Char(' ')] {
            assert!(press(&mut app, key).is_empty());
        }
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            [Effect::Reply(Reply::Confirm(true))]
        );
    }

    #[test]
    fn esc_refuses_a_confirmation() {
        let mut app = operating();
        app.on_worker(WorkerEvent::Confirm(Question::ApplyEdit), Instant::now());
        assert_eq!(
            press(&mut app, KeyCode::Esc),
            [Effect::Reply(Reply::Confirm(false))]
        );
    }

    #[test]
    fn the_diff_and_file_are_kept_for_the_confirmation() {
        let mut app = operating();
        app.on_worker(
            WorkerEvent::Notice(Notice::Diff {
                path: PathBuf::from("/p.nix"),
                old: "a\n".to_owned(),
                new: "a\nb\n".to_owned(),
            }),
            Instant::now(),
        );
        let operation = operation(&app);
        assert_eq!(
            operation.file.as_deref(),
            Some(std::path::Path::new("/p.nix"))
        );
        assert!(operation.diff.as_deref().unwrap().contains("+b"));
        assert!(operation.log.is_empty());
    }

    #[test]
    fn esc_cancels_what_is_running_once() {
        let mut app = operating();
        assert!(matches!(
            press(&mut app, KeyCode::Esc)[0],
            Effect::Cancel { .. }
        ));
        assert!(press(&mut app, KeyCode::Esc).is_empty());
    }

    #[test]
    fn a_failure_keeps_its_cause_until_dismissed_and_then_reloads() {
        let mut app = operating();
        app.on_worker(
            WorkerEvent::Finished(JobResult::Operation(Err("edit error: nope".to_owned()))),
            Instant::now(),
        );
        assert_eq!(
            operation(&app).phase,
            Phase::Failed("edit error: nope".to_owned())
        );
        assert!(press(&mut app, KeyCode::Char('x')).is_empty());
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Browse);
        assert_eq!(submitted(&effects), [JobKind::LoadList]);
    }

    #[test]
    fn choosing_accumulates_pages_and_answers_by_position() {
        let mut app = operating();
        let page = |attrs: &[&str]| attrs.iter().map(|attr| package(attr)).collect();
        app.on_worker(
            WorkerEvent::Choose {
                requested: "x".to_owned(),
                page: page(&["a", "b"]),
                first_index: 0,
                has_more: true,
                offer_search: true,
            },
            Instant::now(),
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('m')),
            [Effect::Reply(Reply::Choice(Choice::More))]
        );
        app.on_worker(
            WorkerEvent::Choose {
                requested: "x".to_owned(),
                page: page(&["c"]),
                first_index: 2,
                has_more: false,
                offer_search: true,
            },
            Instant::now(),
        );
        let Phase::Choosing(choosing) = &operation(&app).phase else {
            panic!("not choosing");
        };
        assert_eq!(choosing.options.len(), 3, "pages accumulate");
        assert_eq!(choosing.highlighted, 2, "the new page is in view");
        press(&mut app, KeyCode::Up);
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            [Effect::Reply(Reply::Choice(Choice::SearchAll))]
        );
    }

    #[test]
    fn the_first_path_is_typed_as_text() {
        let mut app = operating();
        app.on_worker(WorkerEvent::AskPath, Instant::now());
        type_text(&mut app, "/a/q.nix");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            [Effect::Reply(Reply::Path(Some("/a/q.nix".to_owned())))]
        );
    }

    #[test]
    fn reinstall_asks_first_and_can_be_cancelled() {
        let mut app = loaded();
        assert!(press(&mut app, KeyCode::Char('R')).is_empty());
        assert!(matches!(app.screen, Screen::ConfirmReinstall(_)));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.screen, Screen::Browse);

        press(&mut app, KeyCode::Char('R'));
        let effects = press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            submitted(&effects),
            [JobKind::Reinstall("pkgs.git".to_owned())]
        );
    }

    #[test]
    fn details_wait_for_the_selection_to_rest_and_nothing_running() {
        let now = Instant::now();
        let later = now + DETAILS_DELAY * 2;
        let (mut app, _) = App::new(now);
        assert!(app.on_tick(later).is_empty(), "the list is still loading");

        let mut app_loaded = loaded();
        let moved = Instant::now();
        app_loaded.moved_at = moved;
        assert!(app_loaded.on_tick(moved).is_empty());
        let effects = app_loaded.on_tick(moved + DETAILS_DELAY);
        assert_eq!(
            submitted(&effects),
            [JobKind::Details("pkgs.git".to_owned())]
        );

        app_loaded.on_worker(
            WorkerEvent::Finished(JobResult::Details {
                name: "pkgs.git".to_owned(),
                result: Err("offline".to_owned()),
            }),
            moved,
        );
        assert_eq!(
            app_loaded.selected_details(),
            Some(&Details::Failed("offline".to_owned()))
        );
        assert!(
            app_loaded.on_tick(moved + DETAILS_DELAY * 3).is_empty(),
            "a failure is not retried in a loop"
        );
        app.selected = 0;
    }

    #[test]
    fn entries_without_a_catalog_are_not_looked_up() {
        let (mut app, _) = App::new(Instant::now());
        app.on_worker(
            WorkerEvent::Finished(JobResult::List(Ok(PackageList {
                path: PathBuf::from("/p.nix"),
                packages: vec![listed("tool", PackageSource::FlakeInput("tool".to_owned()))],
                warning: None,
            }))),
            Instant::now(),
        );
        assert!(app.on_tick(Instant::now() + DETAILS_DELAY).is_empty());
        assert!(keys(&app).contains(&"r"), "it can still be removed");
    }

    #[test]
    fn narrow_terminals_open_details_on_their_own() {
        let mut app = loaded();
        app.wide = false;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Details);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.screen, Screen::Browse);

        let mut app = loaded();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Browse);
    }

    #[test]
    fn help_closes_with_any_key() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.screen, Screen::Help);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.screen, Screen::Browse);
    }
}
