//! Root dashboard composition for the terminal shell.
//!
//! Workspace modules own their own rendering. This module only composes those
//! workspaces into the shared shell chrome and routes focused/overlay layers.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Paragraph, Tabs, Wrap},
};

use crate::{APPLICATION_NAME, model::GameState};

use super::{
    Shell, View,
    chrome::{render_footer, render_help_overlay, shell_status_line, tab_label},
    fleet, is_bankrupt, map, modal,
    overlays::{
        bankruptcy_text, render_bankruptcy_restart_confirmation, render_outcome_overlay,
        render_world_details_overlay,
    },
    theme,
};

/// Draws the complete dashboard with Ratatui widgets. Crossterm supplies the
/// cross-platform terminal backend and events; Ratatui owns layout and paint.
pub(super) fn render_frame(frame: &mut ratatui::Frame, shell: &mut Shell, state: &GameState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::terminal()), area);
    if let Some(hint) = shell.resize_hint(area.width, area.height) {
        frame.render_widget(
            Paragraph::new(hint)
                .block(
                    Block::default()
                        .borders(theme::THIN_BORDERS)
                        .border_style(theme::border())
                        .title(APPLICATION_NAME)
                        .title_style(theme::title())
                        .style(theme::panel()),
                )
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let [header_area, navigation_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(7),
        Constraint::Length(4),
    ])
    .areas(area);

    let now = state.last_processed_at;
    frame.render_widget(
        Paragraph::new(shell_status_line(state, now, header_area.width)).style(theme::terminal()),
        header_area,
    );

    let views = [
        View::Map,
        View::Trains,
        View::BuyTrains,
        View::Company,
        View::Authority,
        View::Bulletin,
    ];
    let selected = views
        .iter()
        .position(|view| *view == shell.active_view)
        .unwrap_or(0);
    let compact_tabs = navigation_area.width <= 80;
    let titles = views
        .iter()
        .map(|view| Line::from(tab_label(*view, compact_tabs)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Tabs::new(titles)
            .style(theme::navigation())
            .select(selected)
            .highlight_style(theme::active_tab())
            .divider("   "),
        navigation_area,
    );

    if !is_bankrupt(state) && shell.active_view == View::Map && shell.service_workspace.is_open() {
        shell
            .service_workspace
            .render_base(frame, content_area, state);
    } else if !is_bankrupt(state) && shell.active_view == View::Map {
        shell
            .map_workspace
            .render_dashboard(frame, content_area, state);
    } else if !is_bankrupt(state) && shell.active_view == View::Trains {
        shell
            .fleet_workspace
            .render_dashboard(frame, content_area, state, now);
    } else if shell.active_view == View::Trains {
        frame.render_widget(
            Paragraph::new(fleet::render_at(state, now))
                .block(
                    Block::default()
                        .borders(theme::THIN_BORDERS)
                        .border_style(theme::border())
                        .title(shell.active_view.label())
                        .title_style(theme::title())
                        .style(theme::panel()),
                )
                .style(theme::panel())
                .wrap(Wrap { trim: false }),
            content_area,
        );
    } else if shell.active_view == View::Company && !is_bankrupt(state) {
        shell
            .company_workspace
            .render_dashboard(frame, content_area, state);
    } else if shell.active_view == View::BuyTrains && !is_bankrupt(state) {
        shell
            .market_workspace
            .render_dashboard(frame, content_area, state);
    } else if shell.active_view == View::Authority && !is_bankrupt(state) {
        shell
            .authority_workspace
            .render_dashboard(frame, content_area, state, now);
    } else if shell.active_view == View::Bulletin && !is_bankrupt(state) {
        shell.bulletin_workspace.render(frame, content_area, state, now);
    } else {
        let content = if is_bankrupt(state) {
            bankruptcy_text(false)
        } else {
            match shell.active_view {
                View::Map => map::render_at(state, now),
                View::Trains => fleet::render_at(state, now),
                View::BuyTrains => shell.market_workspace.render_text(state),
                View::Company => shell.company_workspace.render_text(state),
                View::Authority => shell.authority_workspace.render_text(state, now),
                View::Bulletin => "Railway Bulletin".into(),
            }
        };
        frame.render_widget(
            Paragraph::new(content)
                .block(
                    Block::default()
                        .borders(theme::THIN_BORDERS)
                        .border_style(theme::border())
                        .title(shell.active_view.label())
                        .title_style(theme::title())
                        .style(theme::panel()),
                )
                .style(theme::panel())
                .wrap(Wrap { trim: false }),
            content_area,
        );
    }

    render_footer(frame, footer_area, shell, state);

    // Focused workflows are a separate presentation layer. The entire app is
    // first muted, then the modal is painted with the normal palette so input
    // ownership is obvious without making the dialog larger or brighter.
    if focused_modal_visible(shell, state) {
        modal::dim_backdrop(frame, area);
        render_focused_modal(frame, content_area, shell, state);
    }

    // Informational overlays can stack above an active workflow (for example
    // Help opened from Dispatch). Each layer dims what is already underneath
    // it, which keeps the topmost interaction visually unambiguous.
    if shell.map_workspace.world_details_visible() {
        modal::dim_backdrop(frame, area);
        render_world_details_overlay(frame, area, state);
    }
    if shell.help_visible {
        modal::dim_backdrop(frame, area);
        render_help_overlay(frame, area, shell, state);
    }
    if shell.outcome_details_open {
        if let Some(outcome) = &shell.action_outcome {
            modal::dim_backdrop(frame, area);
            render_outcome_overlay(frame, area, outcome);
        }
    }
}

fn focused_modal_visible(shell: &Shell, state: &GameState) -> bool {
    shell.authority_workspace.has_modal()
        || shell.fleet_workspace.has_modal()
        || shell.company_workspace.has_modal()
        || (is_bankrupt(state) && shell.restart_confirmation)
        || shell.dispatch_workspace.has_flow()
        || shell.market_workspace.has_modal()
        || (shell.service_workspace.is_open() && shell.service_workspace.has_modal())
}

fn render_focused_modal(
    frame: &mut ratatui::Frame,
    content_area: Rect,
    shell: &mut Shell,
    state: &GameState,
) {
    if shell.authority_workspace.has_modal() {
        shell
            .authority_workspace
            .render_modal(frame, content_area, state);
        return;
    }
    if shell.fleet_workspace.has_modal() {
        shell
            .fleet_workspace
            .render_modal(frame, content_area, state);
        return;
    }
    if shell.company_workspace.has_modal() {
        shell
            .company_workspace
            .render_modal(frame, content_area, state);
        return;
    }
    if is_bankrupt(state) && shell.restart_confirmation {
        render_bankruptcy_restart_confirmation(frame, content_area);
        return;
    }
    if let Some(flow) = shell.dispatch_workspace.flow_mut() {
        flow.render_panel(frame, modal::workflow_rect(content_area), state);
        return;
    }
    if shell.market_workspace.has_modal() {
        shell
            .market_workspace
            .render_modal(frame, content_area, state);
        return;
    }
    if shell.service_workspace.is_open() && shell.service_workspace.has_modal() {
        shell
            .service_workspace
            .render_modal(frame, content_area, state);
    }
}
