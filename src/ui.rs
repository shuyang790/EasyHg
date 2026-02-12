use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};

use crate::app::{App, FocusPanel};
use crate::domain::{ConflictEntry, FileChange, Revision, Shelf};

#[derive(Debug, Clone, Copy)]
pub struct UiRects {
    pub header: Rect,
    pub footer: Rect,
    pub files: Rect,
    pub details: Rect,
    pub revisions: Rect,
    pub bookmarks: Rect,
    pub shelves: Rect,
    pub conflicts: Rect,
    pub log: Rect,
}

impl Default for UiRects {
    fn default() -> Self {
        Self {
            header: Rect::new(0, 0, 0, 0),
            footer: Rect::new(0, 0, 0, 0),
            files: Rect::new(0, 0, 0, 0),
            details: Rect::new(0, 0, 0, 0),
            revisions: Rect::new(0, 0, 0, 0),
            bookmarks: Rect::new(0, 0, 0, 0),
            shelves: Rect::new(0, 0, 0, 0),
            conflicts: Rect::new(0, 0, 0, 0),
            log: Rect::new(0, 0, 0, 0),
        }
    }
}

impl UiRects {
    pub fn panel_rect(&self, panel: FocusPanel) -> Rect {
        match panel {
            FocusPanel::Files => self.files,
            FocusPanel::Revisions => self.revisions,
            FocusPanel::Bookmarks => self.bookmarks,
            FocusPanel::Shelves => self.shelves,
            FocusPanel::Conflicts => self.conflicts,
            FocusPanel::Log => self.log,
        }
    }
}

pub fn compute_ui_rects(root: Rect) -> UiRects {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(root);
    let body = rows[1];

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(body);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(cols[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(36),
            Constraint::Percentage(18),
            Constraint::Percentage(18),
            Constraint::Percentage(28),
        ])
        .split(cols[1]);

    let shelf_conflict = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(right[2]);

    UiRects {
        header: rows[0],
        footer: rows[2],
        files: left[0],
        details: left[1],
        revisions: right[0],
        bookmarks: right[1],
        shelves: shelf_conflict[0],
        conflicts: shelf_conflict[1],
        log: right[3],
    }
}

pub fn render(frame: &mut Frame<'_>, app: &App, rects: &UiRects) {
    let root = frame.area();

    render_header(frame, rects.header, app);
    render_body(frame, rects, app);
    render_footer(frame, rects.footer, app);

    if let Some(confirm) = &app.confirmation {
        let area = centered_rect(70, 25, root);
        frame.render_widget(Clear, area);
        let text = Text::from(vec![
            Line::from(confirm.message.clone()),
            Line::from(""),
            Line::from(format!("Command: {}", confirm.action.command_preview())),
            Line::from(""),
            Line::from("Press y/Enter to confirm, n/Esc to cancel."),
        ]);
        let modal = Paragraph::new(text).block(
            Block::default()
                .title("Confirm Action")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
        frame.render_widget(modal, area);
    }

    if let Some(input) = &app.input {
        let area = centered_rect(70, 20, root);
        frame.render_widget(Clear, area);
        let text = Text::from(vec![
            Line::from(input.title.clone()),
            Line::from(""),
            Line::from(format!("> {}", input.value)),
            Line::from(""),
            Line::from("Enter to submit, Esc to cancel."),
        ]);
        let modal = Paragraph::new(text).block(
            Block::default()
                .title("Input")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(modal, area);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let repo = app
        .snapshot
        .repo_root
        .as_deref()
        .map(short_path)
        .unwrap_or_else(|| "(not in hg repo)".to_string());
    let branch = app.snapshot.branch.as_deref().unwrap_or("unknown branch");
    let title = format!(
        "easyHg | {} | branch: {} | {}",
        repo, branch, app.snapshot.capabilities.version
    );

    let text = Text::from(vec![Line::from(title), Line::from(app.status_line.clone())]);
    let block = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
    frame.render_widget(block, area);
}

fn render_body(frame: &mut Frame<'_>, rects: &UiRects, app: &App) {
    render_files(frame, rects.files, app, app.focus == FocusPanel::Files);
    render_details(frame, rects.details, app);
    render_revisions(
        frame,
        rects.revisions,
        app,
        app.focus == FocusPanel::Revisions,
    );
    render_bookmarks(
        frame,
        rects.bookmarks,
        app,
        app.focus == FocusPanel::Bookmarks,
    );
    render_shelves(frame, rects.shelves, app, app.focus == FocusPanel::Shelves);
    render_conflicts(
        frame,
        rects.conflicts,
        app,
        app.focus == FocusPanel::Conflicts,
    );
    render_log(frame, rects.log, app, app.focus == FocusPanel::Log);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut keys = vec![
        "q quit",
        "Tab panel",
        "j/k select",
        "c commit",
        "b bookmark",
        "u update",
        "p push",
        "P pull",
        "s shelve",
        "U unshelve",
        "m/M resolve",
        "r refresh",
        "? help->log",
    ];
    if app.snapshot.capabilities.has_rebase {
        keys.push("R rebase");
    }
    if app.snapshot.capabilities.has_histedit {
        keys.push("H histedit");
    }
    let line = Paragraph::new(keys.join(" | ")).block(Block::default().borders(Borders::TOP));
    frame.render_widget(line, area);
}

fn render_files(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let items: Vec<ListItem<'_>> = if app.snapshot.files.is_empty() {
        vec![ListItem::new("(clean working directory)")]
    } else {
        app.snapshot
            .files
            .iter()
            .map(file_item)
            .map(ListItem::new)
            .collect()
    };

    let mut state = ListState::default();
    if !app.snapshot.files.is_empty() {
        *state.offset_mut() = app.files_offset;
        state.select(Some(app.files_idx));
    }
    let list = List::new(items)
        .block(panel_block("Files", focused))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_revisions(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let items: Vec<ListItem<'_>> = if app.snapshot.revisions.is_empty() {
        vec![ListItem::new("(no revisions loaded)")]
    } else {
        app.snapshot
            .revisions
            .iter()
            .map(revision_item)
            .map(ListItem::new)
            .collect()
    };

    let mut state = ListState::default();
    if !app.snapshot.revisions.is_empty() {
        *state.offset_mut() = app.rev_offset;
        state.select(Some(app.rev_idx));
    }
    let list = List::new(items)
        .block(panel_block("Commits", focused))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_bookmarks(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let items: Vec<ListItem<'_>> = if app.snapshot.bookmarks.is_empty() {
        vec![ListItem::new("(no bookmarks)")]
    } else {
        app.snapshot
            .bookmarks
            .iter()
            .map(|bookmark| {
                let marker = if bookmark.active { "*" } else { " " };
                format!(
                    "{} {} @{} {}",
                    marker,
                    bookmark.name,
                    bookmark.rev,
                    &bookmark.node.chars().take(12).collect::<String>()
                )
            })
            .map(ListItem::new)
            .collect()
    };

    let mut state = ListState::default();
    if !app.snapshot.bookmarks.is_empty() {
        *state.offset_mut() = app.bookmarks_offset;
        state.select(Some(app.bookmarks_idx));
    }
    let list = List::new(items)
        .block(panel_block("Bookmarks", focused))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_shelves(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let items: Vec<ListItem<'_>> = if app.snapshot.shelves.is_empty() {
        vec![ListItem::new("(no shelves)")]
    } else {
        app.snapshot
            .shelves
            .iter()
            .map(shelf_item)
            .map(ListItem::new)
            .collect()
    };
    let mut state = ListState::default();
    if !app.snapshot.shelves.is_empty() {
        *state.offset_mut() = app.shelves_offset;
        state.select(Some(app.shelves_idx));
    }

    let list = List::new(items)
        .block(panel_block("Shelves", focused))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_conflicts(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let items: Vec<ListItem<'_>> = if app.snapshot.conflicts.is_empty() {
        vec![ListItem::new("(no merge conflicts)")]
    } else {
        app.snapshot
            .conflicts
            .iter()
            .map(conflict_item)
            .map(ListItem::new)
            .collect()
    };
    let mut state = ListState::default();
    if !app.snapshot.conflicts.is_empty() {
        *state.offset_mut() = app.conflicts_offset;
        state.select(Some(app.conflicts_idx));
    }
    let list = List::new(items)
        .block(panel_block("Conflicts", focused))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_details(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let detail = Paragraph::new(app.detail_text.as_str())
        .block(panel_block("Details (Diff/Patch)", false))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn render_log(frame: &mut Frame<'_>, area: Rect, app: &App, focused: bool) {
    let text = if app.log_lines.is_empty() {
        "(command log is empty)".to_string()
    } else {
        app.log_lines.join("\n")
    };
    let paragraph = Paragraph::new(text)
        .block(panel_block("Command Log", focused))
        .wrap(Wrap { trim: false })
        .scroll((app.log_idx as u16, 0));
    frame.render_widget(paragraph, area);

    if !app.log_lines.is_empty() {
        let mut scrollbar_state = ScrollbarState::new(app.log_lines.len()).position(app.log_idx);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let mut block = Block::default().title(title).borders(Borders::ALL);
    if focused {
        block = block.border_style(Style::default().fg(Color::LightCyan));
    }
    block
}

fn file_item(file: &FileChange) -> String {
    format!("{} {}", file.status.code(), file.path)
}

fn revision_item(rev: &Revision) -> String {
    let short = rev.node.chars().take(10).collect::<String>();
    let desc = rev.desc.lines().next().unwrap_or("").to_string();
    format!("@{} {} {} ({})", rev.rev, short, desc, rev.user)
}

fn shelf_item(shelf: &Shelf) -> String {
    if shelf.description.is_empty() {
        shelf.name.clone()
    } else {
        format!("{} {}", shelf.name, shelf.description)
    }
}

fn conflict_item(conflict: &ConflictEntry) -> String {
    let marker = if conflict.resolved { "R" } else { "U" };
    format!("{marker} {}", conflict.path)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn short_path(path: &str) -> String {
    let max = 42usize;
    if path.chars().count() <= max {
        return path.to_string();
    }
    let tail = path
        .chars()
        .rev()
        .take(max.saturating_sub(3))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}
