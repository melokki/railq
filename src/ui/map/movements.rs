//! Live operating movements presented above the Map workspace.
//!
//! This is deliberately a projection of current simulation state. It does not
//! create or persist timetable data; scheduled services can extend the same
//! surface later without changing today's Journey model.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Cell, Paragraph, Row, Table},
};

use crate::{
    model::{GameState, Journey, JourneyPurpose, TrainStatus, UtcSeconds},
    ui::{format, modal, theme},
};

use super::{
    MapWorkspace, network::station_name, operational::journey_next_stop_station_id,
    shared::remaining_seconds,
};

pub(crate) fn render_movements_overlay(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    workspace: &mut MapWorkspace,
) {
    let card = modal::centered_rect(area, 118, 30);

    let mut journeys = state.active_journeys.iter().collect::<Vec<_>>();
    journeys.sort_by_key(|journey| (journey.arrives_at, journey.train_id));

    let mut ready_trains = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter_map(|train| match &train.status {
            TrainStatus::Ready { at } => Some((train, *at)),
            TrainStatus::Travelling { .. } => None,
        })
        .collect::<Vec<_>>();
    ready_trains.sort_by_key(|(train, station_id)| (station_name(state, *station_id), train.id));

    let running_count = journeys.len();
    let ready_count = ready_trains.len();
    let ready_height = if ready_trains.is_empty() {
        3
    } else {
        ready_trains.len().saturating_add(3).min(9) as u16
    };

    // The focused modal body is the card height minus borders, separator, and
    // footer. Reserve a compact summary block plus the READY section, then scroll
    // only the rows that cannot fit. Running movements consume scroll first so the
    // READY section remains stable at the bottom of the modal.
    let body_height = card.height.saturating_sub(4);
    let summary_height = 2;
    let movements_height = body_height
        .saturating_sub(summary_height)
        .saturating_sub(ready_height.saturating_add(1));
    let movement_capacity = if journeys.is_empty() {
        0
    } else {
        usize::from(movements_height.saturating_sub(3))
    };
    let ready_capacity = if ready_trains.is_empty() {
        0
    } else {
        usize::from(ready_height.saturating_sub(3))
    };
    let hidden_movements = journeys.len().saturating_sub(movement_capacity);
    let hidden_ready = ready_trains.len().saturating_sub(ready_capacity);
    let max_scroll = hidden_movements.saturating_add(hidden_ready);
    workspace.movements_scroll_offset = workspace.movements_scroll_offset.min(max_scroll);
    let movement_offset = workspace.movements_scroll_offset.min(hidden_movements);
    let ready_offset = workspace
        .movements_scroll_offset
        .saturating_sub(hidden_movements)
        .min(hidden_ready);

    let footer = modal::shortcut_line(&[
        modal::ModalShortcut::enabled("Esc", modal::ModalAction::Close),
        modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Scroll),
    ]);
    let modal_areas = modal::render_shell(frame, card, "Network Movements", footer);
    let [summary_area, movements_area, separator_area, ready_area] = Layout::vertical([
        Constraint::Length(summary_height),
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(ready_height),
    ])
    .areas(modal_areas.body);

    render_operating_summary(frame, summary_area, running_count, ready_count);
    render_running_trains(
        frame,
        movements_area,
        state,
        now,
        journeys,
        movement_offset,
        running_count,
    );

    modal::render_horizontal_separator(frame, separator_area);
    render_ready_trains(frame, ready_area, state, ready_trains, ready_offset);
}

fn render_operating_summary(
    frame: &mut Frame,
    area: Rect,
    running_count: usize,
    ready_count: usize,
) {
    let operating = running_count > 0;
    let status = if operating {
        "LIVE OPERATIONS"
    } else {
        "NETWORK IDLE"
    };
    let status_style = if operating {
        theme::success()
    } else {
        theme::secondary()
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!("{status} · {running_count} running · {ready_count} ready"),
                status_style,
            ),
            Line::from(""),
        ])
        .style(theme::panel()),
        area,
    );
}

fn render_running_trains(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    journeys: Vec<&Journey>,
    scroll_offset: usize,
    running_count: usize,
) {
    if journeys.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("RUNNING TRAINS · 0", theme::secondary()),
                Line::from(""),
                Line::from("No trains are currently travelling."),
            ])
            .style(theme::panel()),
            area,
        );
        return;
    }

    let [heading_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("RUNNING TRAINS · {running_count}"),
            theme::focused_title(),
        ))
        .style(theme::panel()),
        heading_area,
    );

    let rows = journeys.into_iter().skip(scroll_offset).map(|journey| {
        let next_stop_id = journey_next_stop_station_id(state, journey);
        let next_stop = next_stop_id
            .map(|station_id| station_name(state, station_id))
            .unwrap_or_else(|| "—".into());
        let current_leg = current_leg_label(state, journey, next_stop_id);

        Row::new([
            Cell::from(format!("T{:02}", journey.train_id.get())),
            Cell::from(service_label(state, journey)),
            Cell::from(current_leg),
            Cell::from(next_stop),
            Cell::from(format::clock_time(journey.arrives_at)),
            Cell::from(format::duration(remaining_seconds(journey, now))),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Percentage(32),
            Constraint::Percentage(22),
            Constraint::Length(9),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new([
            "TRAIN",
            "SERVICE",
            "CURRENT LEG",
            "NEXT STOP",
            "ARRIVES",
            "TIME LEFT",
        ])
        .style(theme::table_header())
        .bottom_margin(1),
    )
    .style(theme::panel());

    frame.render_widget(table, table_area);
}

fn render_ready_trains(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    ready_trains: Vec<(&crate::model::Train, crate::model::RailStationId)>,
    scroll_offset: usize,
) {
    if ready_trains.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("READY TRAINS · 0", theme::secondary()),
                Line::from("No trains are ready for dispatch."),
            ])
            .style(theme::panel()),
            area,
        );
        return;
    }

    let ready_count = ready_trains.len();
    let [heading_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("READY TRAINS · {ready_count}"),
            theme::success(),
        ))
        .style(theme::panel()),
        heading_area,
    );

    let rows = ready_trains
        .into_iter()
        .skip(scroll_offset)
        .map(|(train, station_id)| {
            let service = state
                .player_company
                .fleet
                .assigned_service_id(train.id)
                .and_then(|service_id| {
                    state
                        .player_company
                        .passenger_services
                        .iter()
                        .find(|service| service.id == service_id)
                })
                .map(|service| service.name.clone())
                .unwrap_or_else(|| "Unassigned".into());

            Row::new([
                Cell::from(format!("T{:02}", train.id.get())),
                Cell::from(station_name(state, station_id)),
                Cell::from(service),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(55),
            Constraint::Min(12),
        ],
    )
    .header(
        Row::new(["TRAIN", "LOCATION", "SERVICE"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .style(theme::panel());
    frame.render_widget(table, table_area);
}

fn service_label(state: &GameState, journey: &Journey) -> String {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .map(|service| service.name.clone())
        .unwrap_or_else(|| "Unknown".into());

    if journey.purpose == JourneyPurpose::Positioning {
        format!("{service} · POS")
    } else {
        service
    }
}

fn current_leg_label(
    state: &GameState,
    journey: &Journey,
    next_stop_id: Option<crate::model::RailStationId>,
) -> String {
    let current_station_id = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .and_then(|service| service.stop_station_ids.get(journey.current_stop_index))
        .copied()
        .unwrap_or(journey.origin_station_id);
    let next_station_id = next_stop_id.unwrap_or(journey.destination_station_id);

    format!(
        "{} → {}",
        station_name(state, current_station_id),
        station_name(state, next_station_id),
    )
}
