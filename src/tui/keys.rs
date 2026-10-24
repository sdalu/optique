//! The keymap: one key press applied to the app, shared by the event loop
//! and the headless driver.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::draft;
use crate::session::Session;

use super::apply_modal::commit_apply;
use super::{App, Focus, ui};

/// What the caller of `dispatch_key` still has to do about a key.
pub(crate) enum KeyOutcome {
    /// The key was handled entirely by the state update.
    Continue,
    /// The screen needs a hard repaint (Ctrl-L); no state changed.
    Repaint,
    /// The session is over.
    Quit,
}

/// Apply one key press to the app. Everything terminal-related is left to the
/// caller through `KeyOutcome`, so the real event loop and the headless driver
/// share exactly one keymap.
pub(super) fn dispatch_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Ctrl-L: full repaint to clear terminal artifacts — works in every
    // mode (modal, filter, help) without consuming any state.
    if ctrl && key.code == KeyCode::Char('l') {
        return KeyOutcome::Repaint;
    }
    // A key acts on the port the pane shows.
    app.sync_editor();
    // A message lasts until the next key: left up, it would hide the key
    // legend for the rest of the session.
    app.clear_message();

    // Ctrl-C quits from anywhere, before any popup or prompt can take it as
    // a key of its own; asked a second time, at the quit prompt, it keeps
    // the staged edits as a draft.
    if ctrl && key.code == KeyCode::Char('c') {
        if app.quit_confirm {
            return save_draft_and_quit(app);
        }
        return request_quit(app);
    }
    let code = plain_code(key);

    // Modal and quit-confirm grab all keys.
    if app.quit_confirm {
        return handle_quit_confirm_key(app, code);
    }
    if app.modal.is_some() {
        handle_modal_key(app, code);
        return KeyOutcome::Continue;
    }
    if let Some(tab) = app.help_tab {
        handle_help_key(app, tab, code);
        return KeyOutcome::Continue;
    }
    if let Some(help) = &mut app.port_help {
        // Clamped to the real content at draw time.
        match scroll_key(code, help.scroll as usize, app.page.port_help) {
            Some(at) => help.scroll = clamp_u16(at),
            None => app.port_help = None,
        }
        return KeyOutcome::Continue;
    }
    if app.why.is_some() {
        handle_why_key(app, code);
        return KeyOutcome::Continue;
    }
    if let Some(info) = &mut app.opt_info {
        match scroll_key(code, info.scroll as usize, app.page.opt_info) {
            Some(at) => info.scroll = clamp_u16(at),
            None => app.opt_info = None,
        }
        return KeyOutcome::Continue;
    }
    // The bulk prompt takes text input, so it grabs the keys before the
    // filter branch and the plain command keys.
    if app.bulk.is_some() {
        handle_bulk_key(app, key, code);
        return KeyOutcome::Continue;
    }
    if app.focus == Focus::Filter {
        handle_filter_key(app, code);
        return KeyOutcome::Continue;
    }

    handle_command_key(app, code)
}

/// The quit prompt takes every key: `y` keeps the staged edits as a draft,
/// `n` drops them, anything else goes back to the session.
fn handle_quit_confirm_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => return save_draft_and_quit(app),
        // Drop any older draft too: quitting on purpose without saving
        // must not resurrect stale intentions.
        KeyCode::Char('n') | KeyCode::Char('N') => {
            draft::discard(&app.options_dir);
            return KeyOutcome::Quit;
        }
        _ => app.quit_confirm = false,
    }
    KeyOutcome::Continue
}

/// The help popup: a digit or Tab picks a tab, the scroll keys scroll it,
/// any other key closes it.
fn handle_help_key(app: &mut App, tab: usize, code: KeyCode) {
    let n = ui::HELP_TABS.len();
    match code {
        KeyCode::Char(c @ '1'..='9') => {
            let idx = c as usize - '1' as usize;
            if idx < n {
                app.open_help(idx);
            }
        }
        KeyCode::Tab | KeyCode::Right => app.open_help((tab + 1) % n),
        KeyCode::BackTab | KeyCode::Left => app.open_help((tab + n - 1) % n),
        _ => match scroll_key(code, app.help_scroll as usize, app.page.help) {
            Some(at) => app.help_scroll = clamp_u16(at),
            None => {
                app.help_tab = None;
                app.help_scroll = 0;
            }
        },
    }
}

/// The why popup: the scroll keys move its cursor, Enter jumps to the port
/// under it, `r` re-opens why on that port, any other key closes it.
fn handle_why_key(app: &mut App, code: KeyCode) {
    let Some(why) = &mut app.why else { return };
    // The scroll keys move the cursor, which the box keeps in view.
    if let Some(at) = scroll_key(code, why.selected, app.page.why) {
        why.selected = at.min(why.entries().len().saturating_sub(1));
        return;
    }
    match code {
        KeyCode::Enter => {
            let target = why.entries().get(why.selected).cloned().cloned();
            app.why = None;
            if let Some(target) = target {
                app.jump_to_port(&target);
            }
        }
        KeyCode::Char('r') => {
            // Walk the graph: re-open why on the highlighted port. Around
            // a loop this steps from port to port along the cycle.
            if let Some(target) = why.entries().get(why.selected).cloned().cloned() {
                app.why = Some(app.why_for(target));
            }
        }
        _ => app.why = None,
    }
}

/// The bulk prompt, a line of text: Enter runs it, Esc drops it, Ctrl-U
/// clears it (read from `key`, before the folding made it no key).
fn handle_bulk_key(app: &mut App, key: KeyEvent, code: KeyCode) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Ctrl-U clears the line, as in a shell; read before the folding
    // made it no key, and nowhere else.
    if ctrl && key.code == KeyCode::Char('u') {
        if let Some(input) = app.bulk.as_mut() {
            input.clear();
        }
        return;
    }
    match code {
        KeyCode::Esc => app.bulk = None,
        KeyCode::Enter => {
            if let Some(input) = app.bulk.take() {
                app.run_bulk(&input);
            }
        }
        KeyCode::Backspace => {
            if let Some(input) = app.bulk.as_mut() {
                input.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(input) = app.bulk.as_mut() {
                input.push(c);
            }
        }
        _ => {}
    }
}

/// The filter line: typing narrows the list, Enter or an arrow accepts it,
/// Esc clears it.
fn handle_filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.filter.clear();
            app.focus = Focus::List;
            app.rebuild_visible(app.selected_key());
        }
        KeyCode::Enter => app.focus = Focus::List,
        // An arrow accepts the filter as Enter does, and moves on the
        // list it left: type a few letters, then arrow to the port.
        KeyCode::Up | KeyCode::Down => {
            app.focus = Focus::List;
            app.move_selection(if code == KeyCode::Up { -1 } else { 1 });
        }
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
}

/// The command keys, answered whatever pane has the focus; the view keys
/// next ([`handle_view_key`]), then the focused pane's own.
fn handle_command_key(app: &mut App, code: KeyCode) -> KeyOutcome {
    match code {
        KeyCode::Char('q') => return request_quit(app),
        KeyCode::Char('/') => {
            // A fresh filter: the old text goes, and the whole list with it.
            app.focus = Focus::Filter;
            if !app.filter.is_empty() {
                app.filter.clear();
                app.refresh_view();
            }
        }
        KeyCode::Char('?') | KeyCode::F(1) => app.open_help(0),
        KeyCode::Char('a') => app.open_apply_modal(),
        KeyCode::Char('B') => app.open_bulk(),
        // vi's pair: 'u' undoes the last change, whichever port it was on;
        // 'U' takes the selected port back to its saved state.
        KeyCode::Char('u') => app.undo_last(),
        KeyCode::Char('U') => app.reset_selected(Session::revert, "reverted to saved state"),
        // A port whose file takes no edit keeps its state; say why.
        KeyCode::Char('d') => match app
            .selected_key()
            .and_then(|k| app.session.edit_refusal(&k))
        {
            Some(why) => app.flash(&why, true),
            None => app.reset_selected(Session::reset_to_defaults, "reset to port defaults"),
        },
        _ => {
            if !handle_view_key(app, code) {
                match app.focus {
                    Focus::List => handle_list_key(app, code),
                    Focus::Editor => handle_editor_key(app, code),
                    Focus::Filter => {}
                }
            }
        }
    }
    KeyOutcome::Continue
}

/// The keys that change what the list shows or how; false for any other.
fn handle_view_key(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('t') => app.toggle_view(
            |a| &mut a.hide_ok,
            "showing only ports needing attention (t to show all)",
            "showing all ports",
        ),
        KeyCode::Char('V') => {
            // A choice that holds until quit, whatever the terminal's size.
            let shown = !app.about_shown();
            app.about_choice = Some(shown);
            app.about_synced = shown;
            app.flash(
                if shown {
                    "port description shown (V: hide · v: full text)"
                } else {
                    "port description hidden (V: show · v: full text)"
                },
                false,
            );
            // After the flash: a pkg-descr that cannot be read says so.
            app.sync_about();
        }
        KeyCode::Char('m') => app.toggle_view(
            |a| &mut a.mc_relax,
            "make.conf-decided staleness counts as ok (≈, m to undo)",
            "make.conf-decided staleness counts as stale again",
        ),
        KeyCode::Char('L') => {
            // Turning the view off must always work, even once the loop it
            // was showing is gone; turning it on with nothing to show would
            // only empty the list.
            if !app.only_loops && app.loops.is_empty() {
                app.flash("no dependency loop in this closure", false);
            } else {
                app.toggle_view(
                    |a| &mut a.only_loops,
                    "showing only ports caught in a dependency loop (L to show all)",
                    "showing all ports",
                );
            }
        }
        KeyCode::Char('s') => app.toggle_view(
            |a| &mut a.sort_problems_first,
            "sorting problems first",
            "alphabetical order (stable while editing)",
        ),
        KeyCode::Char('w') => app.toggle_view(
            |a| &mut a.warn_mc,
            "flagging options that contradict make.conf OPTIONS_SET/UNSET (≠, w to undo)",
            "make.conf contradictions no longer flagged",
        ),
        _ => return false,
    }
    true
}

/// The key as the keymap reads it, its Ctrl folded in: Ctrl-F and Ctrl-B
/// are PgDn and PgUp, and any other Ctrl-letter is no key at all, so it is
/// never taken for the plain letter (Ctrl-F is not `f`). Ctrl-L and Ctrl-C
/// are answered before this.
fn plain_code(key: KeyEvent) -> KeyCode {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return key.code;
    }
    match key.code {
        KeyCode::Char('f') => KeyCode::PageDown,
        KeyCode::Char('b') => KeyCode::PageUp,
        KeyCode::Char(_) => KeyCode::Null,
        code => code,
    }
}

/// The one scroll key set of every scrolling popup — the help, pkg-help
/// and about, why, option info, the apply preview: `j`/`k` and `↑`/`↓` a
/// row, `g`/`G` and Home/End the first and the last, PgUp/PgDn (Ctrl-B/F
/// folded in) a page of the `rows` the popup last showed. The offset `at`
/// moved, `usize::MAX` for the last (the popup clamps it); None for any
/// other key, which keeps its popup's own meaning.
fn scroll_key(code: KeyCode, at: usize, rows: u16) -> Option<usize> {
    Some(match code {
        KeyCode::Down | KeyCode::Char('j') => at.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
        KeyCode::PageDown => at.saturating_add(page_step(rows)),
        KeyCode::PageUp => at.saturating_sub(page_step(rows)),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => usize::MAX,
        _ => return None,
    })
}

/// A scroll offset kept in a u16 (a Paragraph's), saturated.
pub(super) fn clamp_u16(at: usize) -> u16 {
    at.min(u16::MAX as usize) as u16
}

/// How far a page key moves in a view that showed `rows` rows: one less,
/// so the row at the edge stays in view (and at least one).
pub(super) fn page_step(rows: u16) -> usize {
    (rows as usize).saturating_sub(1).max(1)
}

/// Quit, or ask first when there are staged edits to save as a draft or
/// discard. Shared by Ctrl-C and 'q'.
fn request_quit(app: &mut App) -> KeyOutcome {
    if app.session.dirty() {
        app.quit_confirm = true;
        return KeyOutcome::Continue;
    }
    // Nothing staged: a draft restored and then reverted by hand has
    // nothing left to keep and must not come back on the next launch.
    draft::discard(&app.options_dir);
    KeyOutcome::Quit
}

/// The quit prompt's yes: keep the staged edits as a draft, then quit. A
/// draft that cannot be saved says why and leaves the session open.
fn save_draft_and_quit(app: &mut App) -> KeyOutcome {
    match draft::save(&app.session, &app.options_dir) {
        // No flash on success: the terminal is about to be restored. The
        // next launch announces the draft instead.
        Ok(_) => KeyOutcome::Quit,
        Err(e) => {
            app.quit_confirm = false;
            app.flash(&format!("draft save failed: {e:#}"), true);
            KeyOutcome::Continue
        }
    }
}

fn handle_list_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') | KeyCode::Home => app.select_index(0),
        KeyCode::Char('G') | KeyCode::End => app.select_index(app.visible.len().saturating_sub(1)),
        KeyCode::PageDown => app.move_selection(page_step(app.page.list) as isize),
        KeyCode::PageUp => app.move_selection(-(page_step(app.page.list) as isize)),
        KeyCode::Char('n') => {
            app.jump_problem(1);
        }
        KeyCode::Char('p') => {
            app.jump_problem(-1);
        }
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        // Same meaning as in the option pane, whose title carries the marker.
        KeyCode::Char('h') => app.open_port_help(),
        KeyCode::Char('v') => app.open_port_about(),
        // 'i' is the highlighted option's, and the list highlights none.
        KeyCode::Char('i') => app.flash(
            "i: option details, in the option pane (Enter) — v: port description",
            false,
        ),
        // Space toggles an option, and the list highlights none.
        KeyCode::Char(' ') => app.flash("Enter: edit the options", false),
        // Esc lets go of a filter left on the list.
        KeyCode::Esc if !app.filter.is_empty() => {
            app.filter.clear();
            app.refresh_view();
        }
        KeyCode::Enter | KeyCode::Tab | KeyCode::Right if !app.editor_rows.is_empty() => {
            app.focus = Focus::Editor;
            app.editor_select_first();
        }
        _ => {}
    }
}

fn handle_editor_key(app: &mut App, code: KeyCode) {
    match code {
        // 'h' is the port's own help here, not vi-left: leaving the pane is
        // Esc/Tab/←, which every other overlay uses too.
        KeyCode::Esc | KeyCode::Tab | KeyCode::Left => app.focus = Focus::List,
        KeyCode::Char('h') => app.open_port_help(),
        KeyCode::Char('j') | KeyCode::Down => app.editor_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.editor_move(-1),
        KeyCode::Char('g') | KeyCode::Home => app.editor_steps(-1, usize::MAX),
        KeyCode::Char('G') | KeyCode::End => app.editor_steps(1, usize::MAX),
        KeyCode::PageDown => app.editor_page(1),
        KeyCode::PageUp => app.editor_page(-1),
        // The next port needing attention, its options at once: the cursor
        // starts on its first one.
        KeyCode::Char(c @ ('n' | 'p')) => {
            if app.jump_problem(if c == 'n' { 1 } else { -1 }) {
                app.editor_select_first();
            }
        }
        KeyCode::Char('v') => app.open_port_about(),
        // Space alone toggles: Enter is the list's key into the pane, and
        // pressed once too often it must not flip an option.
        KeyCode::Char(' ') => app.toggle_current(),
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        KeyCode::Char('i') => app.open_opt_info(),
        _ => {}
    }
}

fn handle_modal_key(app: &mut App, code: KeyCode) {
    let Some(modal) = &app.modal else { return };
    if modal.done.is_some() {
        // The list was rebuilt as the apply ended; what it did stays in the
        // status bar until the next key.
        if let Some(done) = app.modal.take().and_then(|m| m.done) {
            let (msg, error) = done.message();
            app.flash(&msg, error);
        }
        return;
    }
    let conflicts = !modal.conflicted.is_empty();
    match code {
        KeyCode::Char('Y') => commit_apply(app, false),
        KeyCode::Char('y') if !conflicts => commit_apply(app, false),
        KeyCode::Char('s') if conflicts => commit_apply(app, true),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.modal = None,
        _ => match scroll_key(code, modal.scroll, app.page.apply) {
            // In entries, clamped at draw time to the last page.
            Some(at) => {
                if let Some(modal) = app.modal.as_mut() {
                    modal.scroll = at;
                }
            }
            // No answer: the preview stays open and says which keys are.
            // Enter by habit must not cancel, and with conflicts a
            // lowercase y must not write.
            None => app.flash(
                if conflicts {
                    "conflicts remain: Y writes anyway · s skips them · n cancels"
                } else {
                    "y writes · n cancels"
                },
                false,
            ),
        },
    }
}
