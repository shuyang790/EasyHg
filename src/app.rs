use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CEvent, EventStream, KeyCode, KeyEvent,
    KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, execute, terminal};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::AppConfig;
use crate::domain::{RepoSnapshot, Revision};
use crate::hg::{CliHgClient, CommandResult, HgAction, HgClient};
use crate::ui;

const LOG_LIMIT: usize = 200;
const MAX_LOG_LINES: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Files,
    Revisions,
    Bookmarks,
    Shelves,
    Conflicts,
    Log,
}

impl FocusPanel {
    pub fn all() -> [Self; 6] {
        [
            Self::Files,
            Self::Revisions,
            Self::Bookmarks,
            Self::Shelves,
            Self::Conflicts,
            Self::Log,
        ]
    }
}

#[derive(Debug, Clone)]
pub enum InputPurpose {
    CommitMessage,
    BookmarkName,
    ShelveName,
}

#[derive(Debug, Clone)]
pub struct InputState {
    pub title: String,
    pub value: String,
    pub purpose: InputPurpose,
}

#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub message: String,
    pub action: HgAction,
}

#[derive(Debug)]
pub enum AppEvent {
    SnapshotLoaded(Result<RepoSnapshot, String>),
    DetailLoaded {
        request_id: u64,
        result: Result<String, String>,
    },
    ActionFinished {
        action: HgAction,
        result: Result<CommandResult, String>,
    },
}

pub struct App {
    pub config: AppConfig,
    pub focus: FocusPanel,
    pub snapshot: RepoSnapshot,
    pub detail_text: String,
    pub log_lines: Vec<String>,
    pub status_line: String,
    pub input: Option<InputState>,
    pub confirmation: Option<PendingConfirmation>,
    pub should_quit: bool,
    pub files_idx: usize,
    pub rev_idx: usize,
    pub bookmarks_idx: usize,
    pub shelves_idx: usize,
    pub conflicts_idx: usize,
    pub log_idx: usize,
    last_refresh: Instant,
    detail_request_id: u64,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    hg: Arc<dyn HgClient>,
}

impl App {
    pub fn new(config: AppConfig) -> Result<Self> {
        let cwd = std::env::current_dir().context("failed reading current directory")?;
        let status_line = format!(
            "Theme: {} | key overrides: {} | q to quit.",
            config.theme,
            config.keybinds.len()
        );
        let hg = Arc::new(CliHgClient::new(cwd)) as Arc<dyn HgClient>;
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let mut app = Self {
            config,
            focus: FocusPanel::Files,
            snapshot: RepoSnapshot::default(),
            detail_text: "Loading…".to_string(),
            log_lines: Vec::new(),
            status_line,
            input: None,
            confirmation: None,
            should_quit: false,
            files_idx: 0,
            rev_idx: 0,
            bookmarks_idx: 0,
            shelves_idx: 0,
            conflicts_idx: 0,
            log_idx: 0,
            last_refresh: Instant::now() - Duration::from_secs(10),
            detail_request_id: 0,
            event_tx,
            event_rx,
            hg,
        };

        if app.config.custom_commands.is_empty() {
            app.append_log("No custom commands configured.");
        } else {
            let lines: Vec<String> = app
                .config
                .custom_commands
                .iter()
                .map(|cmd| {
                    let context = match cmd.context {
                        crate::config::CommandContext::Repo => "repo",
                        crate::config::CommandContext::File => "file",
                        crate::config::CommandContext::Revision => "revision",
                    };
                    format!(
                        "Loaded custom command: {} ({}) [{}] => {}{}",
                        cmd.id,
                        cmd.title,
                        context,
                        cmd.command,
                        if cmd.needs_confirmation {
                            " [confirm]"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            for line in lines {
                app.append_log(line);
            }
        }

        Ok(app)
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode().context("failed enabling raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen, EnableMouseCapture)
            .context("failed entering alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("failed creating terminal")?;
        terminal.clear().ok();

        self.refresh_snapshot();
        self.refresh_detail_for_focus();

        let mut event_stream = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let run_result = loop {
            if let Err(err) = terminal.draw(|f| ui::render(f, self)) {
                break Err(anyhow::anyhow!("terminal draw failed: {err}"));
            }
            if self.should_quit {
                break Ok(());
            }

            tokio::select! {
                _ = tick.tick() => {
                    self.periodic_refresh();
                }
                maybe_ui_event = event_stream.next() => {
                    if let Some(Ok(CEvent::Key(key))) = maybe_ui_event {
                        self.handle_key(key);
                    }
                }
                maybe_app_event = self.event_rx.recv() => {
                    if let Some(app_event) = maybe_app_event {
                        self.handle_app_event(app_event);
                    }
                }
            }
        };

        self.restore_terminal(terminal)?;
        run_result
    }

    fn restore_terminal(&self, mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        disable_raw_mode().ok();
        terminal
            .backend_mut()
            .execute(terminal::LeaveAlternateScreen)
            .ok();
        terminal.backend_mut().execute(DisableMouseCapture).ok();
        terminal.show_cursor().ok();
        Ok(())
    }

    fn periodic_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_secs(7) {
            self.refresh_snapshot();
        }
    }

    fn refresh_snapshot(&mut self) {
        self.last_refresh = Instant::now();
        self.status_line = "Refreshing repository state…".to_string();
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        tokio::spawn(async move {
            let result = hg
                .refresh_snapshot(LOG_LIMIT)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::SnapshotLoaded(result));
        });
    }

    fn refresh_detail_for_focus(&mut self) {
        let request_id = self.detail_request_id.wrapping_add(1);
        self.detail_request_id = request_id;
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        match self.focus {
            FocusPanel::Files => {
                if let Some(file) = self.snapshot.files.get(self.files_idx) {
                    let file_path = file.path.clone();
                    tokio::spawn(async move {
                        let result = hg
                            .file_diff(&file_path)
                            .await
                            .map_err(|err| err.to_string());
                        let _ = tx.send(AppEvent::DetailLoaded { request_id, result });
                    });
                }
            }
            FocusPanel::Revisions => {
                if let Some(rev) = self.snapshot.revisions.get(self.rev_idx) {
                    let rev_num = rev.rev;
                    tokio::spawn(async move {
                        let result = hg
                            .revision_patch(rev_num)
                            .await
                            .map_err(|err| err.to_string());
                        let _ = tx.send(AppEvent::DetailLoaded { request_id, result });
                    });
                }
            }
            _ => {
                self.detail_text = "Select a file or revision to view details.".to_string();
            }
        }
    }

    fn run_action(&mut self, action: HgAction) {
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        self.status_line = format!("Running: {}", action.command_preview());
        tokio::spawn(async move {
            let result = hg.run_action(&action).await.map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::ActionFinished { action, result });
        });
    }

    fn confirm_action(&mut self, action: HgAction, message: impl Into<String>) {
        self.confirmation = Some(PendingConfirmation {
            action,
            message: message.into(),
        });
    }

    fn open_input(&mut self, purpose: InputPurpose, title: impl Into<String>) {
        self.input = Some(InputState {
            title: title.into(),
            value: String::new(),
            purpose,
        });
    }

    fn selected_revision(&self) -> Option<&Revision> {
        self.snapshot.revisions.get(self.rev_idx)
    }

    fn append_log(&mut self, line: impl Into<String>) {
        let now = Local::now().format("%H:%M:%S");
        self.log_lines.push(format!("[{now}] {}", line.into()));
        if self.log_lines.len() > MAX_LOG_LINES {
            let extra = self.log_lines.len() - MAX_LOG_LINES;
            self.log_lines.drain(0..extra);
        }
    }

    fn adjust_indexes(&mut self) {
        if self.files_idx >= self.snapshot.files.len() {
            self.files_idx = self.snapshot.files.len().saturating_sub(1);
        }
        if self.rev_idx >= self.snapshot.revisions.len() {
            self.rev_idx = self.snapshot.revisions.len().saturating_sub(1);
        }
        if self.bookmarks_idx >= self.snapshot.bookmarks.len() {
            self.bookmarks_idx = self.snapshot.bookmarks.len().saturating_sub(1);
        }
        if self.shelves_idx >= self.snapshot.shelves.len() {
            self.shelves_idx = self.snapshot.shelves.len().saturating_sub(1);
        }
        if self.conflicts_idx >= self.snapshot.conflicts.len() {
            self.conflicts_idx = self.snapshot.conflicts.len().saturating_sub(1);
        }
        if self.log_idx >= self.log_lines.len() {
            self.log_idx = self.log_lines.len().saturating_sub(1);
        }
    }

    fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::SnapshotLoaded(result) => match result {
                Ok(snapshot) => {
                    self.snapshot = snapshot;
                    self.adjust_indexes();
                    self.status_line = "Repository state refreshed.".to_string();
                    self.refresh_detail_for_focus();
                    self.append_log("Snapshot refreshed");
                }
                Err(err) => {
                    self.status_line = "Snapshot refresh failed.".to_string();
                    self.append_log(format!("Refresh failed: {err}"));
                }
            },
            AppEvent::DetailLoaded { request_id, result } => {
                if request_id == self.detail_request_id {
                    match result {
                        Ok(text) => {
                            self.detail_text = if text.trim().is_empty() {
                                "No diff output.".to_string()
                            } else {
                                text
                            };
                        }
                        Err(err) => {
                            self.detail_text = format!("Failed loading detail: {err}");
                        }
                    }
                }
            }
            AppEvent::ActionFinished { action, result } => match result {
                Ok(out) => {
                    if out.success {
                        self.status_line = format!("Completed: {}", out.command_preview);
                        self.append_log(format!("OK: {}", out.command_preview));
                    } else {
                        self.status_line = format!("Command failed: {}", out.command_preview);
                        let detail = format!(
                            "{}\n{}\n{}",
                            out.command_preview,
                            out.stdout.trim(),
                            out.stderr.trim()
                        );
                        self.append_log(format!("FAILED: {}", detail.trim()));
                    }
                    self.refresh_snapshot();
                }
                Err(err) => {
                    self.status_line = format!("Command error: {}", action.command_preview());
                    self.append_log(format!("ERROR: {}", err.trim()));
                }
            },
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.handle_confirmation_key(key) || self.handle_input_key(key) {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.append_log(help_text(&self.snapshot.capabilities)),
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('r') => self.refresh_snapshot(),
            KeyCode::Char('d') => self.refresh_detail_for_focus(),
            KeyCode::Char('c') => self.open_input(InputPurpose::CommitMessage, "Commit message"),
            KeyCode::Char('b') => self.open_input(InputPurpose::BookmarkName, "New bookmark"),
            KeyCode::Char('s') => {
                if self.snapshot.capabilities.has_shelve {
                    self.open_input(InputPurpose::ShelveName, "Shelve name");
                } else {
                    self.status_line = "Shelve extension/command unavailable.".to_string();
                }
            }
            KeyCode::Char('p') => self.confirm_action(HgAction::Push, "Push current changes?"),
            KeyCode::Char('P') => self.run_action(HgAction::Pull),
            KeyCode::Char('i') => self.run_action(HgAction::Incoming),
            KeyCode::Char('o') => self.run_action(HgAction::Outgoing),
            KeyCode::Char('u') => self.update_action_for_selection(),
            KeyCode::Char('U') => self.unshelve_selected(),
            KeyCode::Char('m') => self.mark_selected_conflict(true),
            KeyCode::Char('M') => self.mark_selected_conflict(false),
            KeyCode::Char('R') => self.maybe_rebase(),
            KeyCode::Char('H') => self.maybe_histedit(),
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.refresh_snapshot();
                self.refresh_detail_for_focus();
            }
            _ => {}
        }
    }

    fn maybe_rebase(&mut self) {
        if !self.snapshot.capabilities.has_rebase {
            self.status_line = "Rebase extension not enabled.".to_string();
            return;
        }
        if let Some(rev) = self.selected_revision() {
            self.confirm_action(
                HgAction::RebaseSource {
                    source_rev: rev.rev,
                },
                format!("Rebase revision {} onto working parent (.)?", rev.rev),
            );
        }
    }

    fn maybe_histedit(&mut self) {
        if !self.snapshot.capabilities.has_histedit {
            self.status_line = "Histedit extension not enabled.".to_string();
            return;
        }
        if let Some(rev) = self.selected_revision() {
            self.confirm_action(
                HgAction::HisteditBase { base_rev: rev.rev },
                format!("Start histedit from revision {}?", rev.rev),
            );
        }
    }

    fn mark_selected_conflict(&mut self, resolved: bool) {
        if let Some(conflict) = self.snapshot.conflicts.get(self.conflicts_idx) {
            let action = if resolved {
                HgAction::ResolveMark {
                    path: conflict.path.clone(),
                }
            } else {
                HgAction::ResolveUnmark {
                    path: conflict.path.clone(),
                }
            };
            self.run_action(action);
        } else {
            self.status_line = "No conflict selected.".to_string();
        }
    }

    fn unshelve_selected(&mut self) {
        if let Some(shelf) = self.snapshot.shelves.get(self.shelves_idx) {
            self.confirm_action(
                HgAction::Unshelve {
                    name: shelf.name.clone(),
                },
                format!("Unshelve '{}'? This applies shelved changes.", shelf.name),
            );
        } else {
            self.status_line = "No shelf selected.".to_string();
        }
    }

    fn update_action_for_selection(&mut self) {
        match self.focus {
            FocusPanel::Bookmarks => {
                if let Some(bookmark) = self.snapshot.bookmarks.get(self.bookmarks_idx) {
                    self.confirm_action(
                        HgAction::UpdateToBookmark {
                            name: bookmark.name.clone(),
                        },
                        format!("Update working directory to bookmark '{}'?", bookmark.name),
                    );
                }
            }
            _ => {
                if let Some(rev) = self.snapshot.revisions.get(self.rev_idx) {
                    self.confirm_action(
                        HgAction::UpdateToRevision { rev: rev.rev },
                        format!("Update working directory to revision {}?", rev.rev),
                    );
                }
            }
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let panels = FocusPanel::all();
        let pos = panels
            .iter()
            .position(|panel| *panel == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % panels.len()
        } else {
            (pos + panels.len() - 1) % panels.len()
        };
        self.focus = panels[next];
        self.refresh_detail_for_focus();
    }

    fn move_selection(&mut self, delta: isize) {
        let apply_delta = |idx: &mut usize, len: usize, delta: isize| {
            if len == 0 {
                *idx = 0;
                return;
            }
            let current = *idx as isize;
            let next = (current + delta).clamp(0, (len - 1) as isize);
            *idx = next as usize;
        };

        match self.focus {
            FocusPanel::Files => {
                apply_delta(&mut self.files_idx, self.snapshot.files.len(), delta);
                self.refresh_detail_for_focus();
            }
            FocusPanel::Revisions => {
                apply_delta(&mut self.rev_idx, self.snapshot.revisions.len(), delta);
                self.refresh_detail_for_focus();
            }
            FocusPanel::Bookmarks => apply_delta(
                &mut self.bookmarks_idx,
                self.snapshot.bookmarks.len(),
                delta,
            ),
            FocusPanel::Shelves => {
                apply_delta(&mut self.shelves_idx, self.snapshot.shelves.len(), delta)
            }
            FocusPanel::Conflicts => apply_delta(
                &mut self.conflicts_idx,
                self.snapshot.conflicts.len(),
                delta,
            ),
            FocusPanel::Log => apply_delta(&mut self.log_idx, self.log_lines.len(), delta),
        }
    }

    fn handle_confirmation_key(&mut self, key: KeyEvent) -> bool {
        if self.confirmation.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                if let Some(confirm) = self.confirmation.take() {
                    self.run_action(confirm.action);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirmation = None;
                self.status_line = "Action cancelled.".to_string();
            }
            _ => {}
        }
        true
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        if self.input.is_none() {
            return false;
        }

        let mut submit: Option<InputState> = None;
        if let Some(input) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.status_line = "Input cancelled.".to_string();
                }
                KeyCode::Enter => {
                    submit = self.input.take();
                }
                KeyCode::Backspace => {
                    input.value.pop();
                }
                KeyCode::Char(c) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        input.value.push(c);
                    }
                }
                _ => {}
            }
        }

        if let Some(input) = submit {
            let value = input.value.trim();
            if value.is_empty() {
                self.status_line = "Input cannot be empty.".to_string();
                return true;
            }
            match input.purpose {
                InputPurpose::CommitMessage => self.run_action(HgAction::Commit {
                    message: value.to_string(),
                }),
                InputPurpose::BookmarkName => self.run_action(HgAction::BookmarkCreate {
                    name: value.to_string(),
                }),
                InputPurpose::ShelveName => self.run_action(HgAction::ShelveCreate {
                    name: value.to_string(),
                }),
            }
        }
        true
    }
}

fn help_text(caps: &crate::domain::HgCapabilities) -> String {
    let mut text = vec![
        "Keys: q quit | Tab focus | j/k move | r refresh | d reload diff".to_string(),
        "Actions: c commit | b bookmark | u update | p push(confirm) | P pull".to_string(),
        "Remote: i incoming | o outgoing".to_string(),
        "Shelves: s create shelf | U unshelve selected shelf".to_string(),
        "Conflicts: m mark resolved | M mark unresolved".to_string(),
    ];
    if caps.has_rebase {
        text.push("History: R rebase selected revision onto '.'".to_string());
    }
    if caps.has_histedit {
        text.push("History: H histedit from selected revision".to_string());
    }
    text.join(" | ")
}

pub async fn run_app(config: AppConfig) -> Result<()> {
    let mut app = App::new(config)?;
    app.run().await
}
