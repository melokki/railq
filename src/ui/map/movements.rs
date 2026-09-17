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
    model::{GameState, TrainStatus, UtcSeconds},
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
    let card = modal::centered_rect(area, 112, 28);

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

    let ready_height = if ready_trains.is_empty() {
        3
    } else {
        ready_trains.len().saturating_add(3).min(9) as u16
    };

    // The focused modal body is the card height minus borders, separator, and
    // footer. Table headers consume two rows because they include a bottom
    // margin. Scroll only the rows that cannot fit, first live movements and
    // then READY trains, so both sections remain visible and predictable.
    let body_height = card.height.saturating_sub(4);
    let movements_height = body_height.saturating_sub(ready_height.saturating_add(1));
    let movement_capacity = if journeys.is_empty() {
        0
    } else {
        usize::from(movements_height.saturating_sub(2))
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
    let modal_areas = modal::render_shell(frame, card, "Movements", footer);
    let [movements_area, separator_area, ready_area] = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(ready_height),
    ])
    .areas(modal_areas.body);

    if journeys.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("LIVE MOVEMENTS", theme::focused_title()),
                Line::from(""),
                Line::from("No trains are currently travelling."),
            ])
            .style(theme::panel()),
            movements_area,
        );
    } else {
        let rows = journeys.into_iter().skip(movement_offset).map(|journey| {
            let next_stop = journey_next_stop_station_id(state, journey)
                .map(|station_id| station_name(state, station_id))
                .unwrap_or_else(|| "—".into());
            Row::new([
                Cell::from(format::clock_time(journey.arrives_at)),
                Cell::from(format!("T{:02}", journey.train_id.get())),
                Cell::from(format!(
                    "{} → {}",
                    station_name(state, journey.origin_station_id),
                    station_name(state, journey.destination_station_id),
                )),
                Cell::from(next_stop),
                Cell::from(format_eta(remaining_seconds(journey, now))),
            ])
        });

        let table = Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Length(7),
                Constraint::Percentage(38),
                Constraint::Percentage(28),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(["ARRIVAL", "TRAIN", "SERVICE", "NEXT STOP", "ETA"])
                .style(theme::table_header())
                .bottom_margin(1),
        )
        .style(theme::panel());

        frame.render_widget(table, movements_area);
    }

    modal::render_horizontal_separator(frame, separator_area);
    render_ready_trains(frame, ready_area, state, ready_trains, ready_offset);
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
            Row::new([
                Cell::from(format!("T{:02}", train.id.get())),
                Cell::from(station_name(state, station_id)),
            ])
        });
    let table = Table::new(rows, [Constraint::Length(10), Constraint::Min(1)])
        .header(
            Row::new(["TRAIN", "LOCATION"])
                .style(theme::table_header())
                .bottom_margin(1),
        )
        .style(theme::panel());
    frame.render_widget(table, table_area);
}

fn format_eta(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours == 0 {
        format!("{minutes:02}:{seconds:02}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::format_eta;

    #[test]
    fn eta_uses_departure_board_clock_notation() {
        assert_eq!(format_eta(78), "01:18");
        assert_eq!(format_eta(3_723), "1:02:03");
    }
}
