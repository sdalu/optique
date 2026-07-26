mod ui;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::apply::{self, PendingWrite};
use crate::model::origin::PortKey;
use crate::session::{Session, UiStatus};

#[derive(PartialEq)]
pub enum Focus {
    List,
    Editor,
    Filter,
}

/// One navigable line of the options editor.
pub enum EditorRow {
    GroupHeader(usize),
    /// Option name; selectable.
    Option(String),
    ObsoleteHeader,
    /// Option known only to the saved file; not selectable.
    Obsolete(String),
}

pub struct ApplyModal {
    pub writes: Vec<PendingWrite>,
    pub scroll: usize,
    pub conflicted: Vec<PortKey>,
    /// Result text once applied.
    pub done: Option<String>,
}

pub struct App {
    pub session: Session,
    pub options_dir: PathBuf,
    pub visible: Vec<PortKey>,
    pub list_state: ListState,
    pub focus: Focus,
    pub filter: String,
    pub editor_rows: Vec<EditorRow>,
    pub editor_idx: usize,
    pub message: Option<(String, bool)>,
    pub modal: Option<ApplyModal>,
    pub quit_confirm: bool,
    /// Ports hidden because they have no options (status-bar info).
    pub hidden: usize,
}

pub fn run(session: Session, options_dir: PathBuf) -> Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        anyhow::bail!("the TUI needs a real terminal; use `optique scan` or `optique sync` in scripts");
    }
    let hidden = session.ports.values().filter(|p| !p.options.has_options()).count();
    let mut app = App {
        session,
        options_dir,
        visible: Vec::new(),
        list_state: ListState::default(),
        focus: Focus::List,
        filter: String::new(),
        editor_rows: Vec::new(),
        editor_idx: 0,
        message: None,
        modal: None,
        quit_confirm: false,
        hidden,
    };
    app.rebuild_visible(None);
    app.rebuild_editor();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // Modal and quit-confirm grab all keys.
        if app.quit_confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(()),
                _ => app.quit_confirm = false,
            }
            continue;
        }
        if app.modal.is_some() {
            handle_modal_key(app, key.code);
            continue;
        }
        if app.focus == Focus::Filter {
            match key.code {
                KeyCode::Esc => {
                    app.filter.clear();
                    app.focus = Focus::List;
                    app.rebuild_visible(app.selected_key());
                }
                KeyCode::Enter => app.focus = Focus::List,
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.rebuild_visible(None);
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.rebuild_visible(None);
                }
                _ => {}
            }
            app.rebuild_editor();
            continue;
        }

        // Ctrl-C always quits (with confirm if dirty).
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if app.session.dirty() {
                app.quit_confirm = true;
                continue;
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                if app.session.dirty() {
                    app.quit_confirm = true;
                } else {
                    return Ok(());
                }
            }
            KeyCode::Char('/') => app.focus = Focus::Filter,
            KeyCode::Char('a') => app.open_apply_modal(),
            _ => match app.focus {
                Focus::List => handle_list_key(app, key.code),
                Focus::Editor => handle_editor_key(app, key.code),
                Focus::Filter => {}
            },
        }
    }
}

fn handle_list_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') | KeyCode::Home => app.select_index(0),
        KeyCode::Char('G') | KeyCode::End => {
            app.select_index(app.visible.len().saturating_sub(1))
        }
        KeyCode::PageDown => app.move_selection(15),
        KeyCode::PageUp => app.move_selection(-15),
        KeyCode::Char('n') => app.jump_problem(1),
        KeyCode::Char('p') => app.jump_problem(-1),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Tab | KeyCode::Right => {
            if !app.editor_rows.is_empty() {
                app.focus = Focus::Editor;
                app.editor_select_first();
            }
        }
        _ => {}
    }
}

fn handle_editor_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Tab | KeyCode::Left => {
            app.focus = Focus::List
        }
        KeyCode::Char('j') | KeyCode::Down => app.editor_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.editor_move(-1),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_current(),
        KeyCode::Char('d') => {
            if let Some(key) = app.selected_key() {
                app.session.reset_to_defaults(&key);
                app.flash("reset to port defaults", false);
            }
        }
        KeyCode::Char('u') => {
            if let Some(key) = app.selected_key() {
                app.session.revert(&key);
                app.flash("reverted to saved state", false);
            }
        }
        _ => {}
    }
}

fn handle_modal_key(app: &mut App, code: KeyCode) {
    let Some(modal) = app.modal.as_mut() else { return };
    if modal.done.is_some() {
        app.modal = None;
        return;
    }
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let summary = apply::apply(&modal.writes);
            let mut text = format!("{} file(s) written to {}", summary.written, app.options_dir.display());
            for (key, msg) in &summary.failed {
                text.push_str(&format!("\nFAILED {key}: {msg}"));
            }
            modal.done = Some(text);
            app.session.reload_saved(&app.options_dir);
        }
        KeyCode::Char('j') | KeyCode::Down => modal.scroll = modal.scroll.saturating_add(1),
        KeyCode::Char('k') | KeyCode::Up => modal.scroll = modal.scroll.saturating_sub(1),
        _ => app.modal = None,
    }
}

impl App {
    pub fn selected_key(&self) -> Option<PortKey> {
        self.list_state.selected().and_then(|i| self.visible.get(i)).cloned()
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.select_index(next);
    }

    fn select_index(&mut self, idx: usize) {
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(idx.min(self.visible.len() - 1)));
        }
        self.rebuild_editor();
    }

    fn jump_problem(&mut self, dir: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let start = self.list_state.selected().unwrap_or(0) as isize;
        let mut i = start;
        for _ in 0..len {
            i = (i + dir).rem_euclid(len);
            let info = &self.session.ports[&self.visible[i as usize]];
            if self.session.status(info) != UiStatus::Ok {
                self.select_index(i as usize);
                return;
            }
        }
        self.flash("no ports need attention", false);
    }

    /// Rebuild the filtered, problems-first port list.
    pub fn rebuild_visible(&mut self, keep: Option<PortKey>) {
        let filter = self.filter.to_lowercase();
        let mut items: Vec<(UiStatus, PortKey)> = self
            .session
            .ports
            .iter()
            .filter(|(_, info)| info.options.has_options())
            .filter(|(key, info)| {
                filter.is_empty()
                    || key.to_string().to_lowercase().contains(&filter)
                    || info.pkgname.to_lowercase().contains(&filter)
            })
            .map(|(key, info)| (self.session.status(info), key.clone()))
            .collect();
        items.sort();
        self.visible = items.into_iter().map(|(_, k)| k).collect();
        let idx = keep
            .and_then(|k| self.visible.iter().position(|v| *v == k))
            .unwrap_or(0);
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(idx));
        }
    }

    /// Rebuild the editor rows for the selected port: top-level options in
    /// COMPLETE order, then each group, then obsolete saved-only options.
    pub fn rebuild_editor(&mut self) {
        self.editor_rows.clear();
        self.editor_idx = 0;
        let Some(key) = self.selected_key() else { return };
        let info = &self.session.ports[&key];
        let opts = &info.options;
        let grouped: BTreeSet<&str> = opts
            .groups
            .iter()
            .flat_map(|g| g.members.iter().map(String::as_str))
            .collect();
        for opt in &opts.complete {
            if !grouped.contains(opt.as_str()) {
                self.editor_rows.push(EditorRow::Option(opt.clone()));
            }
        }
        for (gi, g) in opts.groups.iter().enumerate() {
            let members: Vec<&String> =
                g.members.iter().filter(|m| opts.complete.contains(*m)).collect();
            if members.is_empty() {
                continue;
            }
            self.editor_rows.push(EditorRow::GroupHeader(gi));
            for m in members {
                self.editor_rows.push(EditorRow::Option(m.clone()));
            }
        }
        if let Some(state) = self.session.state(info) {
            if let Some(saved) = &state.saved {
                let obsolete: Vec<String> = saved
                    .complete
                    .iter()
                    .chain(saved.set.iter())
                    .chain(saved.unset.iter())
                    .filter(|o| !opts.complete.contains(*o))
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if !obsolete.is_empty() {
                    self.editor_rows.push(EditorRow::ObsoleteHeader);
                    for o in obsolete {
                        self.editor_rows.push(EditorRow::Obsolete(o));
                    }
                }
            }
        }
        self.editor_select_first();
    }

    fn editor_select_first(&mut self) {
        self.editor_idx = self
            .editor_rows
            .iter()
            .position(|r| matches!(r, EditorRow::Option(_)))
            .unwrap_or(0);
    }

    fn editor_move(&mut self, dir: isize) {
        if self.editor_rows.is_empty() {
            return;
        }
        let len = self.editor_rows.len() as isize;
        let mut i = self.editor_idx as isize;
        loop {
            i += dir;
            if i < 0 || i >= len {
                return; // stay put at the edges
            }
            if matches!(self.editor_rows[i as usize], EditorRow::Option(_)) {
                self.editor_idx = i as usize;
                return;
            }
        }
    }

    fn toggle_current(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let Some(EditorRow::Option(opt)) = self.editor_rows.get(self.editor_idx) else {
            return;
        };
        let opt = opt.clone();
        // Warn when enabling an option flagged broken/ignored.
        let warn = {
            let info = &self.session.ports[&key];
            let state = self.session.state(info);
            let turning_on = state.map(|s| !s.staged.contains(&opt)).unwrap_or(false);
            if turning_on {
                info.options.defs.get(&opt).and_then(|d| {
                    d.broken
                        .as_ref()
                        .map(|m| format!("warning: {opt} marks this port BROKEN: {m}"))
                        .or_else(|| {
                            d.ignore
                                .as_ref()
                                .map(|m| format!("warning: {opt} marks this port IGNORED: {m}"))
                        })
                })
            } else {
                None
            }
        };
        match self.session.toggle(&key, &opt) {
            Ok(()) => match warn {
                Some(w) => self.flash(&w, true),
                None => self.message = None,
            },
            Err(e) => self.flash(&e, true),
        }
    }

    fn open_apply_modal(&mut self) {
        let conflicted: Vec<PortKey> = self
            .visible
            .iter()
            .filter(|k| self.session.status(&self.session.ports[k]) == UiStatus::Conflict)
            .cloned()
            .collect();
        let staged = self.session.ports.iter().filter_map(|(key, info)| {
            let state = self.session.state(info)?;
            Some((key, info, state.staged.clone()))
        });
        match apply::plan_writes(staged, &self.options_dir) {
            Ok(writes) if writes.is_empty() => {
                self.flash("nothing to write — everything is up to date", false)
            }
            Ok(writes) => {
                self.modal = Some(ApplyModal { writes, scroll: 0, conflicted, done: None })
            }
            Err(e) => self.flash(&format!("{e:#}"), true),
        }
    }

    fn flash(&mut self, msg: &str, error: bool) {
        self.message = Some((msg.to_string(), error));
    }
}
