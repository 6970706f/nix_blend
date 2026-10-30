//! Draws the state of the interface.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use super::app::{
    App, Declared, DeclaredMark, Details, Hint, Operation, PendingReinstall, Phase, Screen,
    SearchState, Selected,
};
use super::logo::{self, LogoSize};
use super::theme::Theme;
use crate::cli::presenter;
use crate::domain::package::Package;

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 10;
/// Width from which the details panel fits next to the list.
const PANEL_FROM_WIDTH: u16 = 90;
const PANEL_WIDTH: u16 = 40;
const NAME: &str = "NIXBLEND";
const SEARCH_PLACEHOLDER: &str = "Search packages...";
const HINT_GAP: &str = "   ";

/// Whether the terminal is wide enough for the details panel.
pub fn is_wide(width: u16) -> bool {
    width >= PANEL_FROM_WIDTH
}

/// Text from files and tools, made safe for a cell: no control characters, and tabs as
/// spaces, since a cell cannot show one.
fn safe(text: &str) -> String {
    presenter::sanitize(text).replace('\t', "    ")
}

/// Shortens `text` from the start: the end of a path is what tells files apart.
fn fit_keeping_end(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let length = text.chars().count();
    if length <= width {
        return text.to_owned();
    }
    let kept = width.saturating_sub(3);
    let tail: String = text.chars().skip(length - kept).collect();
    format!("...{tail}")
}

/// Draws everything.
pub fn draw(frame: &mut Frame, app: &App, theme: &Theme, context: &str) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.text()), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = Paragraph::new(format!(
            "Terminal too small. Needs at least {MIN_WIDTH}x{MIN_HEIGHT}."
        ))
        .style(theme.text())
        .wrap(Wrap { trim: true });
        frame.render_widget(message, area);
        return;
    }

    let outer = Block::bordered()
        .border_set(theme.border())
        .border_style(theme.line())
        .style(theme.text());
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let size = LogoSize::for_terminal(theme, area.height);
    let (_, logo_rows) = size.cells();
    let [header, rule_top, body, rule_bottom, footer] = Layout::vertical([
        Constraint::Length(logo_rows),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    draw_header(frame, app, theme, context, size, header);
    draw_rule(frame, theme, area, rule_top);
    match &app.screen {
        Screen::Browse => draw_browse(frame, app, theme, body),
        Screen::Details => draw_details(frame, app, theme, body),
        Screen::Help => draw_help(frame, theme, body),
        Screen::ConfirmReinstall(pending) => draw_confirm_reinstall(frame, theme, pending, body),
        Screen::Operation(operation) => draw_operation(frame, app, operation, theme, body),
    }
    draw_rule(frame, theme, area, rule_bottom);
    if matches!(app.screen, Screen::Browse) && is_wide(area.width) {
        let x = inner.right() - PANEL_WIDTH - 1;
        let line = theme.line();
        frame
            .buffer_mut()
            .set_string(x, rule_top.y, theme.joint_down(), line);
        frame
            .buffer_mut()
            .set_string(x, rule_bottom.y, theme.joint_up(), line);
    }
    draw_hints(frame, &app.hints(), theme, footer);
}

/// A horizontal divider, joined to the outer border at both ends.
fn draw_rule(frame: &mut Frame, theme: &Theme, outer: Rect, area: Rect) {
    let rule = theme.horizontal().repeat(usize::from(area.width));
    let line = theme.line();
    let buffer = frame.buffer_mut();
    buffer.set_string(area.x, area.y, rule, line);
    buffer.set_string(outer.x, area.y, theme.joint_right(), line);
    buffer.set_string(outer.right() - 1, area.y, theme.joint_left(), line);
}

fn draw_header(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    context: &str,
    size: LogoSize,
    area: Rect,
) {
    let (logo_width, logo_rows) = size.cells();
    // Text needs to keep off the border, while the logo draws right up to it.
    let area = if logo_rows == 1 { padded(area) } else { area };
    let [logo_area, _, text_area] = Layout::horizontal([
        Constraint::Length(logo_width),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(area);
    logo::draw(size, theme, logo_area, frame.buffer_mut());

    let context = safe(context);
    if logo_rows == 1 {
        let [name_area, context_area] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(context.chars().count() as u16),
        ])
        .areas(text_area);
        frame.render_widget(Paragraph::new(NAME).style(theme.strong()), name_area);
        frame.render_widget(Paragraph::new(context).style(theme.muted()), context_area);
        return;
    }

    // The text sits in the middle of the logo's height.
    let top = (logo_rows.saturating_sub(3)) / 2;
    let mut lines = vec![Line::raw(""); usize::from(top)];
    lines.push(Line::styled(NAME, theme.strong()));
    lines.push(Line::styled(context, theme.muted()));
    if let Declared::Loaded(list) = &app.declared {
        lines.push(Line::styled(
            fit_keeping_end(&safe(&list.path.display().to_string()), text_area.width),
            theme.muted(),
        ));
    }
    frame.render_widget(Paragraph::new(lines).style(theme.text()), text_area);
}

/// The hints that fit, whole. The first ones are the actions of the moment and the last ones
/// are how to get help and leave, so what goes is from the middle.
fn fit_hints(hints: &[Hint], width: u16) -> Vec<Hint> {
    let cost = |hints: &[Hint]| -> usize {
        hints
            .iter()
            .map(|(key, label)| key.chars().count() + label.chars().count() + 1)
            .sum::<usize>()
            + hints.len().saturating_sub(1) * HINT_GAP.len()
    };
    let mut fitted = hints.to_vec();
    while cost(&fitted) > usize::from(width) && fitted.len() > 1 {
        // The one before the last two, or the first when only those are left.
        fitted.remove(fitted.len().saturating_sub(3));
    }
    fitted
}

fn draw_hints(frame: &mut Frame, hints: &[Hint], theme: &Theme, area: Rect) {
    let area = padded(area);
    let hints = fit_hints(hints, area.width);
    let mut spans = Vec::new();
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(HINT_GAP, theme.muted()));
        }
        spans.push(Span::styled(*key, theme.strong()));
        spans.push(Span::styled(format!(" {label}"), theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.text()), area);
}

fn draw_browse(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let (list_area, panel_area) = if is_wide(frame.area().width) {
        let [list, divider, panel] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(PANEL_WIDTH),
        ])
        .areas(area);
        let line = vec![Line::raw(theme.vertical()); usize::from(divider.height)];
        frame.render_widget(Paragraph::new(line).style(theme.line()), divider);
        (list, Some(panel))
    } else {
        (area, None)
    };

    let [input, _, title, list, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(padded(list_area));

    draw_input(frame, app, theme, input);
    draw_list_title(frame, app, theme, title);
    draw_list(frame, app, theme, list, panel_area.is_none());
    draw_status(frame, app, theme, status);
    if let Some(panel) = panel_area {
        draw_details(frame, app, theme, panel);
    }
}

/// Keeps text off the borders.
fn padded(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    }
}

fn draw_input(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let line = if app.editing {
        Line::from(vec![
            Span::styled("> ", theme.strong()),
            Span::styled(safe(&app.input), theme.strong()),
            Span::styled("_", theme.strong()),
        ])
    } else if app.input.is_empty() {
        Line::from(vec![
            Span::styled("> ", theme.muted()),
            Span::styled(SEARCH_PLACEHOLDER, theme.muted()),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", theme.muted()),
            Span::styled(safe(&app.input), theme.text()),
        ])
    };
    frame.render_widget(Paragraph::new(line).style(theme.text()), area);
}

fn draw_list_title(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let title = match &app.search {
        Some(search) => format!("RESULTS  {}", safe(&search.term)),
        None => "DECLARED".to_owned(),
    };
    let count = app.row_count();
    let mut spans = vec![Span::styled(title, theme.strong())];
    if count > 0 {
        spans.push(Span::styled(format!("  {count}"), theme.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.text()), area);
}

fn draw_message(frame: &mut Frame, theme: &Theme, text: &str, style: Style, area: Rect) {
    let paragraph = Paragraph::new(safe(text))
        .style(
            style.patch(
                theme
                    .text()
                    .remove_modifier(ratatui::style::Modifier::all()),
            ),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_list(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, narrow: bool) {
    if let Some(search) = &app.search {
        match &search.state {
            SearchState::Running => {
                draw_message(frame, theme, "Searching...", theme.muted(), area);
            }
            SearchState::NotRun => {
                draw_message(frame, theme, "The search did not run.", theme.muted(), area);
            }
            SearchState::Failed(message) => {
                draw_message(frame, theme, message, theme.text(), area);
            }
            SearchState::Done(packages) if packages.is_empty() => {
                let text = format!("No packages found for '{}'.", search.term);
                draw_message(frame, theme, &text, theme.muted(), area);
            }
            SearchState::Done(packages) => {
                draw_results(frame, app, packages, theme, area, narrow);
            }
        }
        return;
    }
    match &app.declared {
        Declared::Loading => draw_message(frame, theme, "Loading...", theme.muted(), area),
        Declared::NotConfigured => draw_message(
            frame,
            theme,
            "No file is managed yet. Search for a package with / and install it with i to choose the file.",
            theme.muted(),
            area,
        ),
        Declared::Failed(message) => draw_message(frame, theme, message, theme.text(), area),
        Declared::Loaded(list) if list.packages.is_empty() => {
            draw_message(
                frame,
                theme,
                "No packages are declared.",
                theme.muted(),
                area,
            );
        }
        Declared::Loaded(list) => {
            let rows = list.packages.iter().map(|package| {
                Row::new(vec![
                    Cell::from(safe(&package.name)),
                    Cell::from(safe(&package.source)).style(theme.muted()),
                ])
                .style(theme.text())
            });
            draw_table(
                frame,
                app,
                theme,
                area,
                rows.collect(),
                vec!["PACKAGE", "SOURCE"],
                vec![Constraint::Min(10), Constraint::Length(14)],
            );
        }
    }
}

fn draw_results(
    frame: &mut Frame,
    app: &App,
    packages: &[Package],
    theme: &Theme,
    area: Rect,
    narrow: bool,
) {
    let rows = packages.iter().map(|package| {
        let mark = match app.mark_of(package) {
            DeclaredMark::Yes => "declared".to_owned(),
            DeclaredMark::Elsewhere(source) => format!("in {source}"),
            DeclaredMark::No => String::new(),
        };
        let version = package.metadata.version.as_deref().unwrap_or("");
        let mut cells = vec![
            Cell::from(safe(&package.identity.qualified())),
            Cell::from(safe(version)).style(theme.muted()),
            Cell::from(safe(&mark)).style(theme.muted()),
        ];
        if narrow && area.width > 70 {
            let description = package.metadata.description.as_deref().unwrap_or("");
            cells.push(Cell::from(safe(description)).style(theme.muted()));
        }
        Row::new(cells).style(theme.text())
    });
    let (headers, widths) = if narrow && area.width > 70 {
        (
            vec!["PACKAGE", "VERSION", "STATE", "DESCRIPTION"],
            vec![
                Constraint::Length(24),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Min(10),
            ],
        )
    } else {
        (
            vec!["PACKAGE", "VERSION", "STATE"],
            vec![
                Constraint::Min(14),
                Constraint::Length(12),
                Constraint::Length(12),
            ],
        )
    };
    draw_table(frame, app, theme, area, rows.collect(), headers, widths);
}

fn draw_table(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    area: Rect,
    rows: Vec<Row>,
    headers: Vec<&str>,
    widths: Vec<Constraint>,
) {
    let header = Row::new(headers).style(theme.muted());
    let table = Table::new(rows, widths)
        .header(header)
        .style(theme.text())
        .row_highlight_style(theme.selected())
        .highlight_symbol("> ");
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_status(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let text = match &app.progress {
        Some((label, seconds)) => format!("{label}... {seconds}s"),
        None if app.is_searching() => "Esc to stop the search".to_owned(),
        None => String::new(),
    };
    frame.render_widget(Paragraph::new(safe(&text)).style(theme.muted()), area);
}

/// `Label: value` lines, as the command line shows them.
fn field_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| match line.split_once(": ") {
            Some((label, value)) => Line::from(vec![
                Span::styled(format!("{label}: "), theme.muted()),
                Span::styled(safe(value), theme.text()),
            ]),
            None => Line::styled(safe(line), theme.text()),
        })
        .collect()
}

fn draw_details(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let area = padded(area);
    let mut lines = vec![Line::styled("DETAILS", theme.strong()), Line::raw("")];
    match (app.selected_row(), app.selected_details()) {
        (None, _) => lines.push(Line::styled("Nothing selected.", theme.muted())),
        (Some(_), Some(Details::Loaded(info))) => {
            lines.extend(field_lines(&presenter::package_info(info), theme));
        }
        (Some(_), Some(Details::Failed(message))) => {
            lines.extend(known_fields(app, theme));
            lines.push(Line::raw(""));
            lines.push(Line::styled(safe(message), theme.text()));
        }
        (Some(_), Some(Details::Loading)) => {
            lines.extend(known_fields(app, theme));
            lines.push(Line::raw(""));
            lines.push(Line::styled("Loading...", theme.muted()));
        }
        (Some(_), None) => lines.extend(known_fields(app, theme)),
    }
    let details = Paragraph::new(lines)
        .style(theme.text())
        .wrap(Wrap { trim: false });
    frame.render_widget(details, area);
}

/// What the list already knows about the selected row, without asking anything.
fn known_fields(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    match app.selected_row() {
        Some(Selected::Result(package)) => field_lines(&presenter::search_result(package), theme),
        Some(Selected::Declared(listed)) => {
            let text = format!(
                "Attribute: {}\nSource: {}\nDeclared: Yes\n",
                listed.name, listed.source
            );
            field_lines(&text, theme)
        }
        None => Vec::new(),
    }
}

fn draw_help(frame: &mut Frame, theme: &Theme, area: Rect) {
    let keys: [(&str, &str); 12] = [
        ("/", "Search packages"),
        ("Up, Down", "Move in the list"),
        ("PgUp, PgDn", "Move faster"),
        ("Enter", "Details of the selected package"),
        ("i", "Install the selected search result"),
        ("r", "Remove the selected declaration"),
        (
            "R",
            "Reinstall: validate and rebuild, without changing the file",
        ),
        ("Esc", "Back, or cancel what is running"),
        ("y, n", "Answer a question"),
        ("?", "These keys"),
        ("q", "Quit"),
        ("Ctrl+C", "Quit from anywhere, ending what is running"),
    ];
    let mut lines = vec![Line::styled("KEYS", theme.strong()), Line::raw("")];
    lines.extend(keys.iter().map(|(key, meaning)| {
        Line::from(vec![
            Span::styled(format!("{key:<12}"), theme.strong()),
            Span::styled(*meaning, theme.text()),
        ])
    }));
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .wrap(Wrap { trim: false }),
        padded(area),
    );
}

fn draw_confirm_reinstall(
    frame: &mut Frame,
    theme: &Theme,
    pending: &PendingReinstall,
    area: Rect,
) {
    let lines = vec![
        Line::from(vec![
            Span::styled("REINSTALL  ", theme.strong()),
            Span::styled(safe(&pending.package), theme.text()),
        ]),
        Line::from(vec![
            Span::styled("Source: ", theme.muted()),
            Span::styled(safe(&pending.source), theme.text()),
        ]),
        Line::raw(""),
        Line::styled(
            "This validates the configuration and rebuilds the system. The file is not changed, and the package is not necessarily downloaded or compiled again.",
            theme.text(),
        ),
        Line::raw(""),
        Line::styled("Rebuild now?", theme.strong()),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .wrap(Wrap { trim: false }),
        padded(area),
    );
}

fn draw_operation(frame: &mut Frame, app: &App, operation: &Operation, theme: &Theme, area: Rect) {
    let area = padded(area);
    let [content, prompt] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(prompt_height(operation, area)),
    ])
    .areas(area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{}  ", operation.verb), theme.strong()),
            Span::styled(safe(&operation.package), theme.text()),
        ]),
        Line::from(vec![
            Span::styled("Source: ", theme.muted()),
            Span::styled(safe(&operation.source), theme.text()),
        ]),
    ];
    if let Some(file) = &operation.file {
        lines.push(Line::from(vec![
            Span::styled("File: ", theme.muted()),
            Span::styled(safe(&file.display().to_string()), theme.text()),
        ]));
    }
    lines.push(Line::raw(""));
    for entry in &operation.log {
        lines.push(Line::styled(safe(entry), theme.muted()));
    }
    if let Some(diff) = &operation.diff {
        lines.push(Line::raw(""));
        lines.extend(diff_lines(diff, theme));
    }
    if let Phase::Choosing(choosing) = &operation.phase {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("No exact match for '{}'.", safe(&choosing.requested)),
            theme.text(),
        ));
        for (index, package) in choosing.options.iter().enumerate() {
            let row = safe(&presenter::one_line(package));
            let style = if index == choosing.highlighted {
                theme.selected()
            } else {
                theme.text()
            };
            let marker = if index == choosing.highlighted {
                "> "
            } else {
                "  "
            };
            lines.push(Line::styled(format!("{marker}{row}"), style));
        }
    }

    let scroll = operation_scroll(operation, &lines, content.height);
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .scroll((scroll, 0)),
        content,
    );
    draw_operation_prompt(frame, app, operation, theme, prompt);
}

/// Keeps the highlighted option in view while choosing, and otherwise follows the keys.
fn operation_scroll(operation: &Operation, lines: &[Line], height: u16) -> u16 {
    let max = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(height);
    if let Phase::Choosing(choosing) = &operation.phase {
        let options_start = lines.len().saturating_sub(choosing.options.len());
        let target = u16::try_from(options_start + choosing.highlighted + 1).unwrap_or(u16::MAX);
        return target.saturating_sub(height).min(max);
    }
    operation.scroll.min(max)
}

fn prompt_height(operation: &Operation, area: Rect) -> u16 {
    let width = usize::from(area.width.max(1));
    let text = match &operation.phase {
        Phase::Confirming(question) => presenter::question(question).chars().count(),
        Phase::Failed(message) => message.chars().count() + message.matches('\n').count() * width,
        Phase::AskingPath(_) => 2 * width,
        Phase::Choosing(_) => return 0,
        _ => width,
    };
    let rows = text.div_ceil(width).max(1) + 1;
    u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(1))
}

fn draw_operation_prompt(
    frame: &mut Frame,
    app: &App,
    operation: &Operation,
    theme: &Theme,
    area: Rect,
) {
    let lines = match &operation.phase {
        Phase::Running => {
            let text = match &app.progress {
                Some((label, seconds)) => format!("{label}... {seconds}s"),
                None => "Working...".to_owned(),
            };
            vec![Line::raw(""), Line::styled(safe(&text), theme.muted())]
        }
        Phase::Confirming(question) => vec![
            Line::raw(""),
            Line::styled(safe(&presenter::question(question)), theme.strong()),
        ],
        Phase::AskingPath(path) => vec![
            Line::raw(""),
            Line::styled("Path to the file to manage:", theme.strong()),
            Line::from(vec![
                Span::styled("> ", theme.strong()),
                Span::styled(safe(path), theme.text()),
                Span::styled("_", theme.strong()),
            ]),
        ],
        Phase::Choosing(_) => Vec::new(),
        Phase::Succeeded => vec![Line::raw(""), Line::styled("Done.", theme.strong())],
        Phase::Failed(message) => {
            let mut lines = vec![Line::raw("")];
            lines.extend(
                message
                    .lines()
                    .map(|line| Line::styled(safe(line), theme.strong())),
            );
            lines
        }
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// In shades of the same gray: what is added stands out, and what is removed recedes.
fn diff_lines(diff: &str, theme: &Theme) -> Vec<Line<'static>> {
    diff.lines()
        .map(|line| {
            let style =
                if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
                    theme.muted()
                } else if line.starts_with('+') {
                    theme.strong()
                } else if line.starts_with('-') {
                    theme
                        .muted()
                        .add_modifier(ratatui::style::Modifier::CROSSED_OUT)
                } else {
                    theme.text()
                };
            Line::styled(safe(line), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::list_packages::{ListedPackage, PackageList};
    use crate::application::ports::{Notice, Question};
    use crate::application::search_packages::SearchResults;
    use crate::domain::package::{PackageIdentity, PackageMetadata, PackageSource};
    use crate::tui::theme::ColorSupport;
    use crate::tui::worker::{JobResult, ListFailure, WorkerEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Color;
    use std::path::PathBuf;
    use std::time::Instant;

    fn theme() -> Theme {
        Theme::new(ColorSupport::TrueColor, true)
    }

    fn render_with(app: &App, theme: &Theme, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, app, theme, "NixOS / host"))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text_of(buffer: &Buffer) -> String {
        let width = usize::from(buffer.area.width);
        buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        text_of(&render_with(app, &theme(), width, height))
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

    fn package(attr: &str) -> Package {
        Package {
            identity: PackageIdentity::nixpkgs(attr),
            metadata: PackageMetadata {
                pname: Some(attr.to_owned()),
                version: Some("1.0".to_owned()),
                description: Some(format!("The {attr} tool")),
                ..PackageMetadata::default()
            },
        }
    }

    fn loaded() -> App {
        let (mut app, _) = App::new(Instant::now());
        app.on_worker(
            WorkerEvent::Finished(JobResult::List(Ok(PackageList {
                path: PathBuf::from("/etc/nixos/p.nix"),
                packages: vec![
                    listed("firefox", PackageSource::Nixpkgs),
                    listed(
                        "vscode",
                        PackageSource::Channel {
                            alias: "unstable".to_owned(),
                            input: "nixpkgs-unstable".to_owned(),
                        },
                    ),
                ],
                warning: None,
            }))),
            Instant::now(),
        );
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
    }

    fn searched(attrs: &[&str]) -> App {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);
        let id = app.search.as_ref().map_or(0, |search| search.job);
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

    #[test]
    fn the_main_screen_shows_identity_list_and_keys() {
        let text = render(&loaded(), 100, 30);
        for expected in [
            "NIXBLEND",
            "NixOS / host",
            "/etc/nixos/p.nix",
            "Search packages...",
            "DECLARED",
            "PACKAGE",
            "SOURCE",
            "firefox",
            "nixpkgs",
            "vscode",
            "unstable",
            "DETAILS",
            "/ Search",
            "r Remove",
            "q Quit",
        ] {
            assert!(text.contains(expected), "{expected} not in:\n{text}");
        }
    }

    #[test]
    fn the_selected_row_is_inverted() {
        let buffer = render_with(&loaded(), &theme(), 100, 30);
        let text = text_of(&buffer);
        let row = text
            .lines()
            .position(|line| line.contains("> firefox"))
            .expect("the first row is selected");
        let start = usize::from(buffer.area.width) * row;
        let inverted = buffer.content()[start..start + 100]
            .iter()
            .any(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED));
        assert!(inverted);
    }

    #[test]
    fn only_the_logo_and_nothing_else_is_blue() {
        let buffer = render_with(&loaded(), &theme(), 100, 30);
        let (logo_width, logo_rows) = LogoSize::Medium.cells();
        for (index, cell) in buffer.content().iter().enumerate() {
            let x = index % 100;
            let y = index / 100;
            let in_logo = (1..=usize::from(logo_width)).contains(&x)
                && (1..=usize::from(logo_rows)).contains(&y);
            if in_logo {
                continue;
            }
            for color in [cell.fg, cell.bg] {
                if let Color::Rgb(red, green, blue) = color {
                    assert!(
                        red == green && green == blue,
                        "colored cell at {x},{y}: {color:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_logo_is_drawn_at_its_corner() {
        let buffer = render_with(&loaded(), &theme(), 100, 30);
        let logo_cells = buffer.content()[..1000]
            .iter()
            .filter(|cell| ["▀", "▄", "█"].contains(&cell.symbol()))
            .count();
        assert!(logo_cells > 15, "{logo_cells}");
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(fit_keeping_end("/a/b.nix", 20), "/a/b.nix");
        assert_eq!(fit_keeping_end("/long/path/to/b.nix", 10), "...o/b.nix");
        assert_eq!(fit_keeping_end("abc", 0), "...");
    }

    #[test]
    fn hints_that_do_not_fit_go_whole_from_the_middle() {
        let hints: Vec<Hint> = vec![
            ("/", "Search"),
            ("r", "Remove"),
            ("R", "Reinstall"),
            ("Enter", "Details"),
            ("?", "Keys"),
            ("q", "Quit"),
        ];
        assert_eq!(fit_hints(&hints, 200), hints);
        let fitted = fit_hints(&hints, 40);
        assert_eq!(fitted[0].0, "/");
        assert_eq!(fitted.last().unwrap().0, "q");
        assert!(fitted.iter().any(|(key, _)| *key == "?"));
        assert!(fitted.len() < hints.len());
        assert_eq!(fit_hints(&hints, 1).len(), 1);

        let text = render(&loaded(), 60, 16);
        assert!(text.contains("q Quit"), "{text}");
        assert!(text.contains("? Keys"), "{text}");
    }

    #[test]
    fn dividers_join_the_borders() {
        let text = render(&loaded(), 100, 30);
        for joint in ['├', '┤', '┬', '┴'] {
            assert!(text.contains(joint), "{joint} not in:\n{text}");
        }
        assert!(!render(&loaded(), 60, 30).contains('┬'));
    }

    #[test]
    fn borders_are_square() {
        let text = render(&loaded(), 100, 30);
        assert!(text.starts_with('┌'));
        assert!(text.contains('┘'));
        for rounded in ['╭', '╮', '╰', '╯'] {
            assert!(!text.contains(rounded));
        }
    }

    #[test]
    fn the_panel_gives_way_on_narrow_terminals() {
        assert!(render(&loaded(), 100, 30).contains("DETAILS"));
        assert!(!render(&loaded(), 60, 30).contains("DETAILS"));
    }

    #[test]
    fn every_size_can_be_drawn() {
        let mut app = searched(&["foo", "bar"]);
        for width in [1, 10, 39, 40, 41, 60, 89, 90, 120, 250] {
            for height in [1, 5, 9, 10, 11, 19, 20, 24, 33, 34, 70] {
                render_with(&app, &theme(), width, height);
            }
        }
        press(&mut app, KeyCode::Char('i'));
        app.on_worker(
            WorkerEvent::Notice(Notice::Diff {
                path: PathBuf::from("/p.nix"),
                old: "a\n".to_owned(),
                new: "a\nb\n".to_owned(),
            }),
            Instant::now(),
        );
        app.on_worker(WorkerEvent::Confirm(Question::ApplyEdit), Instant::now());
        for width in [1, 40, 60, 100] {
            for height in [1, 10, 11, 12, 30] {
                render_with(&app, &theme(), width, height);
            }
        }
    }

    #[test]
    fn a_tiny_terminal_says_so() {
        assert!(render(&loaded(), 30, 8).contains("too small"));
    }

    #[test]
    fn plain_terminals_still_work() {
        let plain = Theme::new(ColorSupport::None, false);
        let buffer = render_with(&loaded(), &plain, 100, 30);
        let text = text_of(&buffer);
        assert!(text.starts_with('+'));
        assert!(text.contains("NIX"));
        assert!(text.contains("firefox"));
        assert!(text.is_ascii(), "{text}");
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn empty_and_failed_states_are_explained() {
        assert!(render(&searched(&[]), 100, 30).contains("No packages found for 'x'."));

        let (mut app, _) = App::new(Instant::now());
        assert!(render(&app, 100, 30).contains("Loading..."));
        app.on_worker(
            WorkerEvent::Finished(JobResult::List(Err(ListFailure::NotConfigured))),
            Instant::now(),
        );
        assert!(render(&app, 100, 30).contains("No file is managed yet"));
        app.on_worker(
            WorkerEvent::Finished(JobResult::List(Err(ListFailure::Other(
                "config error: bad".to_owned(),
            )))),
            Instant::now(),
        );
        assert!(render(&app, 100, 30).contains("config error: bad"));
    }

    #[test]
    fn typing_shows_the_text_and_the_right_keys() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('i'));
        let text = render(&app, 100, 30);
        assert!(text.contains("> i_"));
        assert!(text.contains("Enter Search"));
        assert!(!text.contains("q Quit"));
    }

    #[test]
    fn an_operation_shows_everything_before_the_question() {
        let mut app = searched(&["foo"]);
        press(&mut app, KeyCode::Char('i'));
        app.on_worker(
            WorkerEvent::Notice(Notice::PackageFound(package("foo"))),
            Instant::now(),
        );
        app.on_worker(
            WorkerEvent::Notice(Notice::Diff {
                path: PathBuf::from("/etc/nixos/p.nix"),
                old: "a\n".to_owned(),
                new: "a\nfoo\n".to_owned(),
            }),
            Instant::now(),
        );
        app.on_worker(WorkerEvent::Confirm(Question::ApplyEdit), Instant::now());
        let text = render(&app, 100, 30);
        for expected in [
            "INSTALL  foo",
            "Source: nixpkgs",
            "File: /etc/nixos/p.nix",
            "Found: foo (1.0)",
            "+foo",
            "Apply this change?",
            "y Yes",
            "n No",
        ] {
            assert!(text.contains(expected), "{expected} not in:\n{text}");
        }
    }

    #[test]
    fn a_failure_shows_its_cause() {
        let mut app = searched(&["foo"]);
        press(&mut app, KeyCode::Char('i'));
        app.on_worker(
            WorkerEvent::Finished(JobResult::Operation(Err(
                "edit error: the file cannot be changed safely: nope".to_owned(),
            ))),
            Instant::now(),
        );
        let text = render(&app, 100, 30);
        assert!(
            text.contains("the file cannot be changed safely: nope"),
            "{text}"
        );
        assert!(text.contains("Enter Back"));
    }

    #[test]
    fn control_characters_never_reach_a_cell() {
        let mut app = searched(&["foo"]);
        press(&mut app, KeyCode::Char('i'));
        app.on_worker(
            WorkerEvent::Finished(JobResult::Operation(
                Err("bad\x1b[31m text\u{7}".to_owned()),
            )),
            Instant::now(),
        );
        let buffer = render_with(&app, &theme(), 100, 30);
        assert!(!text_of(&buffer).contains('\x1b'));
    }

    #[test]
    fn help_lists_the_keys() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('?'));
        let text = render(&app, 100, 30);
        for expected in ["KEYS", "Ctrl+C", "Reinstall", "Esc Back"] {
            assert!(text.contains(expected), "{expected} not in:\n{text}");
        }
    }

    #[test]
    fn results_show_their_state_and_details() {
        let mut app = searched(&["firefox", "foo"]);
        let text = render(&app, 110, 30);
        assert!(text.contains("declared"), "{text}");
        assert!(text.contains("Description: The firefox tool"), "{text}");
        press(&mut app, KeyCode::Down);
        assert!(render(&app, 110, 30).contains("Description: The foo tool"));
    }
}
