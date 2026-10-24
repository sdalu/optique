//! The port list pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use crate::model::origin::PortKey;
use crate::session::UiStatus;
use crate::tui::{App, Focus};

use super::{fit_end, fit_start, fit_title, pane_border};

fn status_marker(status: &UiStatus) -> (&'static str, Color) {
    match status {
        UiStatus::Conflict => ("✗", Color::Red),
        UiStatus::Unconfigured => ("?", Color::Yellow),
        UiStatus::Stale => ("!", Color::LightRed),
        UiStatus::McDeviation => ("≠", Color::Magenta),
        UiStatus::Ok => ("✓", Color::DarkGray),
    }
}

/// The row between the problem ports and the `ok` ok ones, `width` wide.
fn divider_item(ok: usize, width: usize) -> ListItem<'static> {
    let mut text = format!("── {ok} ok ");
    let fill = width.saturating_sub(text.chars().count());
    text.push_str(&"─".repeat(fill));
    ListItem::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )))
}

pub(super) fn draw_port_list(f: &mut Frame, app: &mut App, area: Rect) {
    // The columns a row has for its name and badges, after its two markers
    // and the blank after them.
    let name_room = (area.width as usize).saturating_sub(2 + 3);
    let mut items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|key| port_row(app, key, name_room))
        .collect();
    if let Some(d) = app.divider {
        let width = area.width.saturating_sub(2) as usize;
        items.insert(d, divider_item(app.visible.len() - d, width));
    }

    let room = area.width.saturating_sub(2) as usize;
    let title = list_title(app, room);
    let (border_style, border_type) =
        pane_border(app.focus == Focus::List || app.focus == Focus::Filter);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title)
        .border_style(border_style);
    // The filter on the bottom border, at its right: with the cursor only
    // while it is being typed, as text once left on the list. Too long, it
    // loses its start, so the end being typed stays in view.
    let typing = app.focus == Focus::Filter;
    if typing || !app.filter.is_empty() {
        let cursor = if typing { "▏" } else { "" };
        let label = fit_start(" filter: ", &format!("{}{cursor}", app.filter), " ", room);
        block = block.title_bottom(Line::from(label).right_aligned());
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    // The selection is an index among the ports; the widget counts rows,
    // the divider among them. Its scroll offset is in rows and kept.
    let mut rows = ListState::default()
        .with_offset(app.list_state.offset())
        .with_selected(app.list_state.selected().map(|i| app.list_row(i)));
    f.render_stateful_widget(list, area, &mut rows);
    *app.list_state.offset_mut() = rows.offset();
    app.page.list = area.height.saturating_sub(2);
}

/// One row of the list: the state marker, the edited mark, the name in the
/// style of its severity, and its badges, in `name_room` columns.
fn port_row(app: &App, key: &PortKey, name_room: usize) -> ListItem<'static> {
    let info = &app.session.ports[key];
    let raw = app.session.status(info);
    let status = app.effective_status(info);
    let blacklisted = app.is_blacklisted(info);
    // A port silenced by the mc_relax view keeps a hint marker — but
    // a blacklisted one is silenced for its own reason (the ⊘ below).
    let (marker, color) = if status == UiStatus::Ok && raw != UiStatus::Ok && !blacklisted {
        ("≈", Color::DarkGray)
    } else {
        status_marker(&status)
    };
    // The second column: staged edits, whatever the state.
    let edited = if app.session.edited(info) { "*" } else { " " };
    // The port name itself carries the severity: red bold for a port
    // that will not build (BROKEN/IGNORE), red for conflicting staged
    // options, dim for blacklisted ports.
    let blocked = crate::session::is_blocked(info);
    let name_style = if blacklisted {
        Style::default().fg(Color::DarkGray)
    } else if blocked {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if status == UiStatus::Conflict {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    let mut badges = Vec::new();
    if blacklisted {
        badges.push(Span::styled(" ⊘", Style::default().fg(Color::DarkGray)));
    }
    if blocked {
        badges.push(Span::styled(" ⚠", Style::default().fg(Color::Red)));
    }
    if app.loop_of(key).is_some() {
        badges.push(Span::styled(" ∞", Style::default().fg(Color::Red)));
    }
    // Options the port gained since its file was written, in the
    // style of their N in the option pane: shown whatever the
    // state, so a port -m or the m view settles still says it.
    if let Some((added, _)) = app.session.stale_delta(info).filter(|(a, _)| !a.is_empty()) {
        badges.push(Span::styled(
            format!(" +{}", added.len()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let badge_width: usize = badges.iter().map(Span::width).sum();
    let name = fit_port_name(key, name_room.saturating_sub(badge_width));
    let mut spans = vec![
        Span::styled(marker, Style::default().fg(color)),
        Span::styled(format!("{edited} "), Style::default().fg(Color::Cyan)),
        Span::styled(name, name_style),
    ];
    spans.extend(badges);
    ListItem::new(Line::from(spans))
}

/// The list's title in `room` columns: the counts and the view, the
/// optionless count dropped first, then the view's long wording.
fn list_title(app: &App, room: usize) -> String {
    let count = if app.hide_ok {
        format!("{}/{}", app.visible.len(), app.listable)
    } else {
        format!("{}", app.visible.len())
    };
    // The view the list is in, long and short: kept before the optionless
    // count, and shortened before it is cut.
    let (mut view, mut short) = (String::new(), String::new());
    if app.only_loops {
        view.push_str(" — in a dependency loop");
        short.push_str(" — in a loop");
    }
    if app.hide_ok {
        view.push_str(" — needs attention");
        short.push_str(" — attention");
    }
    let fits = |t: &str| Line::from(t).width() <= room;
    // The ports with no options are in the closure but never listed; their
    // count goes first when the title does not fit, then the view's long
    // wording, and only then is the rest cut.
    let full = format!(" Ports ({count} · {} optionless){view} ", app.hidden);
    let long = format!(" Ports ({count}){view} ");
    let brief = format!(" Ports ({count}){short} ");
    if app.hidden > 0 && fits(&full) {
        full
    } else if fits(&long) {
        long
    } else {
        // A cut never ends on a view's dash: `in a loop…`, not `in a loop —…`.
        fit_title(&brief, room).replace(" —…", "…")
    }
}

/// A list row's `category/name[@flavor]` in `room` columns: whole when it
/// fits, else the category is cut first (`multim…/ffmpeg@default`, then
/// `…/ffmpeg@default`), and only then the end of the name and flavor.
pub(crate) fn fit_port_name(key: &PortKey, room: usize) -> String {
    let full = key.to_string();
    let width = |s: &str| Line::from(s).width();
    if width(&full) <= room {
        return full;
    }
    let (category, rest) = full.split_once('/').unwrap_or(("", full.as_str()));
    // `…/` and the rest, with what is left of the category before them.
    match room.checked_sub(width(rest) + 2) {
        Some(keep) => {
            let head: String = category.chars().take(keep).collect();
            format!("{head}…/{rest}")
        }
        None => format!("…/{}", fit_end(rest, room.saturating_sub(2))),
    }
}
