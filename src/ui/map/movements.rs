//! Live operating movements presented above the Map workspace.
//!
//! This is deliberately a projection of current simulation state. It does not
//! create or persist timetable data; scheduled services can extend the same
//! surface later without changing today's Journey model.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::Line,
    widgets::{Cell, Paragraph, Row, Table},
};

use crate::{
    model::{GameState, UtcSeconds},
    ui::{format, modal, theme},
};

use super::{
    network::station_name,
    operational::journey_next_stop_station_id,
    shared::remaining_seconds,
};

pub(crate) fn render_movements_overlay(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
) {
    let card = modal::centered_rect(area, 112, 28);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "Movements",
        modal::shortcut_line(&[("M/Esc", "close")]),
    );

    let mut journeys = state.active_journeys.iter().collect::<Vec<_>>();
    journeys.sort_by_key(|journey| (journey.arrives_at, journey.train_id));

    if journeys.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("LIVE MOVEMENTS", theme::focused_title()),
                Line::from(""),
                Line::from("No trains are currently travelling."),
                Line::styled(
                    "READY trains remain available through Manual Dispatch.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel()),
            modal_areas.body,
        );
        return;
    }

    let rows = journeys.into_iter().map(|journey| {
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

    frame.render_widget(table, modal_areas.body);
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
