//! Shared shell chrome: status line, navigation labels, footer shortcuts, and help overlay.

use ratatui::{
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    APPLICATION_NAME,
    model::{GameState, TrainStatus, UtcSeconds},
};

use super::{Shell, View, format, is_bankrupt, modal, theme};

#[derive(Clone, Debug, Eq, PartialEq)]
struct FooterShortcut {
    key: String,
    action: String,
    enabled: bool,
}

impl FooterShortcut {
    fn enabled(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            enabled: true,
        }
    }

    fn disabled(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            enabled: false,
        }
    }
}

pub(super) fn footer_height(shell: &Shell) -> u16 {
    if shell.action_outcome.is_some() || shell.notice.is_some() {
        3
    } else {
        2
    }
}

pub(super) fn render_footer(
    frame: &mut ratatui::Frame,
    area: Rect,
    shell: &mut Shell,
    state: &GameState,
) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::footer_border())
        .style(theme::panel());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let feedback = if let Some(outcome) = &shell.action_outcome {
        Some(Line::styled(
            format!("✓ {}", outcome.summary),
            theme::success(),
        ))
    } else {
        shell
            .notice
            .as_ref()
            .map(|notice| Line::styled(notice.clone(), theme::warning()))
    };

    let action_y = inner.y.saturating_add(inner.height.saturating_sub(1));
    if let Some(feedback) = feedback {
        if inner.height >= 2 {
            frame.render_widget(
                Paragraph::new(feedback).style(theme::panel()),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
        }
    }

    let action_area = Rect::new(inner.x, action_y, inner.width, 1);
    let outcome_shortcut = (shell.action_outcome.is_some() && !shell.outcome_details_open)
        .then(|| FooterShortcut::enabled("i", "Details"));
    let reserved = outcome_shortcut
        .as_ref()
        .map(|shortcut| shortcut_width(shortcut).saturating_add(1) as u16)
        .unwrap_or(0);
    let mut shortcuts = contextual_controls(shell, state, inner.width.saturating_sub(reserved));
    if let Some(shortcut) = outcome_shortcut {
        shortcuts.insert(0, shortcut);
    }

    let (contextual, global): (Vec<_>, Vec<_>) = shortcuts
        .into_iter()
        .partition(|shortcut| !is_utility_shortcut(shortcut));
    render_action_strip(frame, action_area, &contextual, &global);
}

fn render_action_strip(
    frame: &mut ratatui::Frame,
    area: Rect,
    contextual: &[FooterShortcut],
    global: &[FooterShortcut],
) {
    if area.width == 0 {
        return;
    }

    let global_width = shortcut_line_width(global)
        .min(usize::from(area.width)) as u16;
    let contextual_width = if global.is_empty() {
        area.width
    } else {
        area.width.saturating_sub(global_width.saturating_add(1))
    };

    if contextual_width > 0 && !contextual.is_empty() {
        frame.render_widget(
            Paragraph::new(shortcut_line(contextual)).style(theme::panel()),
            Rect::new(area.x, area.y, contextual_width, 1),
        );
    }

    if global_width > 0 && !global.is_empty() {
        frame.render_widget(
            Paragraph::new(shortcut_line(global))
                .style(theme::panel())
                .alignment(Alignment::Right),
            Rect::new(
                area.x.saturating_add(area.width.saturating_sub(global_width)),
                area.y,
                global_width,
                1,
            ),
        );
    }
}

fn shortcut_width(shortcut: &FooterShortcut) -> usize {
    shortcut.key.chars().count() + shortcut.action.chars().count() + 3
}

fn shortcut_line_width(shortcuts: &[FooterShortcut]) -> usize {
    shortcuts
        .iter()
        .map(shortcut_width)
        .sum::<usize>()
        .saturating_add(shortcuts.len().saturating_sub(1))
}

fn push_shortcut_if_fits(
    shortcuts: &mut Vec<FooterShortcut>,
    shortcut: FooterShortcut,
    width: u16,
) {
    let added_separator = if shortcuts.is_empty() { 0 } else { 1 };
    let next_width = shortcut_line_width(shortcuts)
        .saturating_add(added_separator)
        .saturating_add(shortcut_width(&shortcut));
    if next_width <= usize::from(width) {
        shortcuts.push(shortcut);
    }
}

fn shortcut_line(shortcuts: &[FooterShortcut]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, shortcut) in shortcuts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", theme::shortcut_action()));
        }
        let key_style = if shortcut.enabled {
            theme::shortcut_key()
        } else {
            theme::shortcut_disabled()
        };
        let action_style = if shortcut.enabled {
            theme::shortcut_action()
        } else {
            theme::shortcut_disabled()
        };
        spans.push(Span::styled(format!("[{}]", shortcut.key), key_style));
        spans.push(Span::styled(format!(" {}", shortcut.action), action_style));
    }
    Line::from(spans)
}

fn is_utility_shortcut(shortcut: &FooterShortcut) -> bool {
    matches!(shortcut.key.as_str(), "?" | "Q")
}

fn contextual_controls(shell: &mut Shell, state: &GameState, width: u16) -> Vec<FooterShortcut> {
    let compact = width <= 78;
    let wide = width >= 104;

    if shell.help_visible {
        let mut actions = vec![
            FooterShortcut::enabled(if compact { "↑↓" } else { "↑↓/JK" }, "Scroll"),
            FooterShortcut::enabled("PgUp/PgDn", "Page"),
            FooterShortcut::enabled("?/Esc", "Close"),
        ];
        actions.push(FooterShortcut::enabled("Q", "Quit"));
        return actions;
    }
    if shell.map_workspace.world_details_visible() {
        let mut actions = shell
            .map_workspace
            .shortcuts(state, compact)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect::<Vec<_>>();
        actions.push(FooterShortcut::enabled("?", "Help"));
        actions.push(FooterShortcut::enabled("Q", "Quit"));
        return actions;
    }
    if shell.active_view == View::Trains && shell.fleet_workspace.has_nickname_editor() {
        return shell
            .fleet_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect();
    }
    if shell.outcome_details_open {
        return vec![
            FooterShortcut::enabled("i", "Close"),
            FooterShortcut::enabled("Esc", "Close"),
        ];
    }
    if shell.active_view == View::Company && shell.company_workspace.has_modal() {
        return shell
            .company_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect();
    }
    if is_bankrupt(state) {
        return if shell.restart_confirmation {
            vec![
                FooterShortcut::enabled("Enter", "Restart"),
                FooterShortcut::enabled("Esc", "Cancel"),
                FooterShortcut::enabled("Q", "Quit"),
            ]
        } else {
            vec![
                FooterShortcut::enabled("R", "Safe restart"),
                FooterShortcut::enabled("?", "Help"),
                FooterShortcut::enabled("Q", "Quit"),
            ]
        };
    }

    let mut actions = if let Some(shortcuts) = shell.dispatch_workspace.shortcuts(compact, wide) {
        shortcuts
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::Trains {
        shell
            .fleet_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::Map && shell.service_workspace.is_open() {
        shell
            .service_workspace
            .footer_shortcuts(compact, wide, state)
            .into_iter()
            .map(|(key, action, enabled)| {
                if enabled {
                    FooterShortcut::enabled(key, action)
                } else {
                    FooterShortcut::disabled(key, action)
                }
            })
            .collect()
    } else if shell.active_view == View::Map {
        shell
            .map_workspace
            .shortcuts(state, compact)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::Company {
        shell
            .company_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::Authority {
        shell
            .authority_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::Bulletin {
        shell
            .bulletin_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else if shell.active_view == View::BuyTrains {
        shell
            .market_workspace
            .shortcuts(state, compact, wide)
            .into_iter()
            .map(|shortcut| FooterShortcut {
                key: shortcut.key,
                action: shortcut.action,
                enabled: shortcut.enabled,
            })
            .collect()
    } else {
        vec![FooterShortcut::enabled("Enter", "Details")]
    };

    // Keep utility actions predictable without forcing the task-specific
    // controls to wrap. At narrow widths they disappear only when they do not
    // fit; every shortcut continues to work even when omitted from the hint.
    push_shortcut_if_fits(&mut actions, FooterShortcut::enabled("?", "Help"), width);
    push_shortcut_if_fits(&mut actions, FooterShortcut::enabled("Q", "Quit"), width);
    actions
}

pub(super) fn shell_status_line(
    state: &GameState,
    now: UtcSeconds,
    width: u16,
) -> Line<'static> {
    let ready = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .count();
    let travelling = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Travelling { .. }))
        .count();
    let next_arrival = nearest_eta(state, now).unwrap_or_else(|| "—".into());

    if width >= 100 {
        shell_status_wide(state, ready, travelling, &next_arrival, width)
    } else {
        shell_status_compact(state, ready, travelling, &next_arrival, width)
    }
}

fn shell_status_wide(
    state: &GameState,
    ready: usize,
    travelling: usize,
    next_arrival: &str,
    width: u16,
) -> Line<'static> {
    let company = shorten(&state.player_company.name, 30);
    let cash = format::money(state.player_company.funds);
    let fleet = format!("{ready} ready · {travelling} travelling");

    let mut left = vec![
        Span::styled(APPLICATION_NAME, theme::shell_brand()),
        Span::raw("  "),
        Span::styled(company, theme::shell_identity()),
    ];
    let right = vec![
        Span::styled("Cash ", theme::shell_metric_label()),
        Span::styled(cash, theme::shell_metric_value()),
        Span::raw("   "),
        Span::styled("Fleet ", theme::shell_metric_label()),
        Span::styled(fleet, theme::shell_metric_value()),
        Span::raw("   "),
        Span::styled("Next ", theme::shell_metric_label()),
        Span::styled(next_arrival.to_owned(), theme::shell_metric_value()),
    ];

    let left_width = Line::from(left.clone()).width();
    let right_width = Line::from(right.clone()).width();
    let available = usize::from(width);
    let gap = available.saturating_sub(left_width + right_width).max(2);
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    Line::from(left)
}

fn shell_status_compact(
    state: &GameState,
    ready: usize,
    travelling: usize,
    next_arrival: &str,
    width: u16,
) -> Line<'static> {
    let company = shorten(&state.player_company.name, 16);
    let cash = format::money(state.player_company.funds);
    let fleet = format!("R{ready}/T{travelling}");

    let mut left = vec![
        Span::styled(APPLICATION_NAME, theme::shell_brand()),
        Span::raw("  "),
        Span::styled(company, theme::shell_identity()),
    ];
    let right = vec![
        Span::styled(cash, theme::shell_metric_value()),
        Span::raw("   "),
        Span::styled(fleet, theme::shell_metric_value()),
        Span::raw("   "),
        Span::styled("Next ", theme::shell_metric_label()),
        Span::styled(next_arrival.to_owned(), theme::shell_metric_value()),
    ];

    let left_width = Line::from(left.clone()).width();
    let right_width = Line::from(right.clone()).width();
    let available = usize::from(width);
    let gap = available.saturating_sub(left_width + right_width).max(2);
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    Line::from(left)
}

fn nearest_eta(state: &GameState, now: UtcSeconds) -> Option<String> {
    state
        .active_journeys
        .iter()
        .map(|journey| {
            journey
                .arrives_at
                .unix_seconds()
                .saturating_sub(now.unix_seconds())
                .max(0) as u64
        })
        .min()
        .map(format_remaining_time)
}

fn format_remaining_time(seconds: u64) -> String {
    format::duration(seconds)
}

pub(super) fn tab_label(view: View, compact: bool, bulletin_unread: u64) -> String {
    let label = match (view, compact) {
        (View::Trains, true) => "Trn",
        (View::BuyTrains, true) => "Mkt",
        (View::Company, true) => "Co",
        (View::Authority, true) => "Auth",
        (View::Bulletin, true) => "News",
        (View::Trains, false) => "Fleet",
        (View::BuyTrains, false) => "Market",
        _ => view.label(),
    };
    let mut title = format!("{} {label}", view.number());
    if view == View::Bulletin && bulletin_unread > 0 {
        let badge = if bulletin_unread > 9 {
            "9+".into()
        } else {
            bulletin_unread.to_string()
        };
        title.push_str(&format!(" [{badge}]"));
    }
    title
}

fn shorten(value: &str, max_characters: usize) -> String {
    let mut characters = value.chars();
    let shortened = characters.by_ref().take(max_characters).collect::<String>();
    if characters.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

pub(super) const HELP_PAGE_STEP: usize = 5;

pub(super) fn help_lines(shell: &Shell, state: &GameState) -> Vec<String> {
    let mut lines = vec![
        "Navigation".into(),
        "1 Map   2 Fleet   3 Market   4 Company   5 Authority   6 Bulletin".into(),
        "Primary workspaces use number keys only; letter keys remain available for contextual actions.".into(),
        String::new(),
    ];

    if shell.active_view == View::Company && shell.company_workspace.has_modal() {
        lines.extend(shell.company_workspace.help_lines(state));
        return lines;
    }

    if is_bankrupt(state) {
        lines.extend([
            "Current · Bankruptcy".into(),
            "r Review a safe restart".into(),
            "Enter Confirm restart when the review is open".into(),
            "Esc Cancel restart review".into(),
        ]);
        return lines;
    }

    if shell.map_workspace.world_details_visible() {
        lines.extend(shell.map_workspace.help_lines(state));
        return lines;
    }

    if let Some(dispatch_lines) = shell.dispatch_workspace.help_lines() {
        lines.extend(dispatch_lines);
        return lines;
    }

    if shell.active_view == View::BuyTrains {
        if let Some(market_lines) = shell.market_workspace.help_lines() {
            lines.extend(market_lines);
            return lines;
        }
    }

    if shell.active_view == View::Map && shell.service_workspace.is_open() {
        lines.extend(shell.service_workspace.help_lines(state));
        return lines;
    }

    match shell.active_view {
        View::Map => lines.extend(shell.map_workspace.help_lines(state)),
        View::Trains => {
            lines.extend(shell.fleet_workspace.help_lines(state));
        }
        View::BuyTrains => {
            lines.extend([
                "Current · Market".into(),
                "↑↓ / jk Select Train model".into(),
                "Enter Buy selected Train".into(),
                String::new(),
                "Purchase price is not the whole decision: keep enough cash for access and fuel."
                    .into(),
            ]);
        }
        View::Company => {
            lines.extend(shell.company_workspace.help_lines(state));
        }
        View::Authority => {
            lines.extend(shell.authority_workspace.help_lines());
        }
        View::Bulletin => {
            lines.extend(shell.bulletin_workspace.help_lines());
        }
    }

    lines.extend([
        String::new(),
        "Tip".into(),
        "The footer is contextual: it only shows actions that matter right now.".into(),
    ]);
    lines
}

pub(super) fn render_help_overlay(
    frame: &mut ratatui::Frame,
    area: Rect,
    shell: &Shell,
    state: &GameState,
) {
    let card = modal::centered_rect(area, 96, 30);
    let footer = if card.width < 76 {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("Esc/?", modal::ModalAction::Close),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Page),
            modal::ModalShortcut::enabled("Esc/?", modal::ModalAction::Close),
            modal::ModalShortcut::enabled("Q", modal::ModalAction::Quit),
        ])
    };
    let modal_areas = modal::render_shell(frame, card, "Keyboard Help", footer);

    let lines = help_lines(shell, state);
    let visible_lines = usize::from(modal_areas.body.height);
    let max_offset = lines.len().saturating_sub(visible_lines.max(1));
    let offset = shell.help_offset.min(max_offset);
    let content = lines
        .into_iter()
        .skip(offset)
        .take(visible_lines)
        .map(|line| {
            if line == "Navigation"
                || line == "Next step"
                || line == "Tip"
                || line.starts_with("Current ·")
            {
                Line::styled(line, theme::focused_title())
            } else {
                Line::from(line)
            }
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(content)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        modal_areas.body,
    );
}
