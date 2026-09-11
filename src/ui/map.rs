//! Text presentation of the Region map.
//!
//! The map reads the public Rail Network and the Player Company's operating
//! state without changing either. Markers make connection state understandable
//! in terminals where colour is unavailable.

use std::fmt::Write;

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    model::{
        GameState, Journey, Money, RailStation, RailStationId, Train, TrainStatus, UtcSeconds,
    },
    ui::theme,
};

/// Presentation-only selection for the connected Rail Station list.
///
/// The selected stable ID, rather than a list index, keeps the inspector on
/// the same Rail Station when the game is reconciled or the terminal resizes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StationSelection {
    selected_station_id: Option<RailStationId>,
    list_state: ListState,
    page_size: usize,
}

impl StationSelection {
    /// Returns the selected connected Rail Station after reconciling its ID.
    pub fn selected_station_id(&mut self, state: &GameState) -> Option<RailStationId> {
        self.synchronize(state);
        self.selected_station_id
    }

    /// Moves through connected Rail Stations without changing the Rail Network
    /// or Passenger Demand.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let stations = &state.region.rail_authority.rail_network.rail_stations;
        let Some(selected) = self.list_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(stations.len().saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(stations.len().saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    fn synchronize(&mut self, state: &GameState) {
        let stations = &state.region.rail_authority.rail_network.rail_stations;
        let previous_index = self.list_state.selected().unwrap_or(0);
        let selected = self
            .selected_station_id
            .and_then(|station_id| stations.iter().position(|station| station.id == station_id))
            .or_else(|| {
                (!stations.is_empty())
                    .then_some(previous_index.min(stations.len().saturating_sub(1)))
            });
        if let Some(index) = selected {
            self.selected_station_id = Some(stations[index].id);
        } else {
            self.selected_station_id = None;
            *self.list_state.offset_mut() = 0;
        }
        self.list_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(station) = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .get(index)
        else {
            return;
        };
        self.selected_station_id = Some(station.id);
        self.list_state.select(Some(index));
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }

    fn keep_compact_selection_visible(&mut self, visible_items: usize) {
        let Some(selected) = self.list_state.selected() else {
            return;
        };
        let visible_items = visible_items.max(1);
        let offset = self.list_state.offset();
        if selected < offset {
            *self.list_state.offset_mut() = selected;
        } else if selected >= offset.saturating_add(visible_items) {
            *self.list_state.offset_mut() =
                selected.saturating_add(1).saturating_sub(visible_items);
        }
    }
}

/// Renders the Map's connected Rail Station list and selected-station
/// inspector. Compact terminals use a focused inspector page opened with
/// Enter; wide terminals keep both panels visible.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut StationSelection,
    details_open: bool,
) {
    selection.synchronize(state);
    if area.width >= 96 && area.height >= 14 {
        let [list_area, inspector_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(38)])
                .spacing(1)
                .areas(area);
        render_station_list(frame, list_area, state, selection, true);
        render_station_inspector(frame, inspector_area, state, selection, false);
    } else if details_open {
        render_station_inspector(frame, area, state, selection, true);
    } else {
        render_station_list(frame, area, state, selection, true);
    }
}

fn render_station_list(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut StationSelection,
    focused: bool,
) {
    let visible_items = usize::from(area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);
    selection.keep_compact_selection_visible(visible_items);
    let stations = &state.region.rail_authority.rail_network.rail_stations;
    let items = stations
        .iter()
        .map(|station| {
            let ready = ready_trains(state, station.id).len();
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{:02}] ", station.id.get()), theme::secondary()),
                Span::raw(station_name(state, station.id)),
                Span::styled(format!("  {ready} READY"), theme::secondary()),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block("Rail Stations · connected", focused))
        .highlight_style(theme::selected_row())
        .highlight_symbol("> ")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, &mut selection.list_state);
}

fn render_station_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut StationSelection,
    focused: bool,
) {
    let selected = selected_station(state, selection);
    let title = selected.map_or_else(
        || "Selected Rail Station".to_owned(),
        |station| {
            format!(
                "Rail Station {:02} · {}",
                station.id.get(),
                station_name(state, station.id)
            )
        },
    );
    let mut lines = Vec::new();
    if let Some(station) = selected {
        let ready = ready_trains(state, station.id);
        lines.push(Line::styled(
            format!("Ready Trains ({})", ready.len()),
            theme::title(),
        ));
        if ready.is_empty() {
            lines.push(Line::styled(
                "No READY Trains at this Rail Station.",
                theme::secondary(),
            ));
        } else {
            lines.extend(ready.into_iter().map(|train| {
                Line::from(vec![
                    Span::styled(
                        format!("Train {:02}  ", train.id.get()),
                        theme::primary_value(),
                    ),
                    Span::raw(train.model_name.clone()),
                ])
            }));
        }
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "Directional Waiting Passengers",
            theme::title(),
        ));
        for destination in state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .filter(|destination| destination.id != station.id)
        {
            let destination_name = station_name(state, destination.id);
            match state.origin_destination_demand.iter().find(|pool| {
                pool.origin_station_id == station.id
                    && pool.destination_station_id == destination.id
            }) {
                Some(pool) => lines.push(Line::from(vec![
                    Span::styled(format!("→ {destination_name:<12}"), theme::secondary()),
                    Span::raw(format!(
                        "{} waiting · +{}/h",
                        pool.waiting_passengers,
                        pool.passenger_arrival_rate_per_hour.passengers_per_hour()
                    )),
                ])),
                None => lines.push(Line::from(vec![
                    Span::styled(format!("→ {destination_name:<12}"), theme::secondary()),
                    Span::styled("Waiting Passengers unavailable", theme::warning()),
                ])),
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::styled(
            if focused {
                "Esc · Rail Stations    D · dispatch"
            } else {
                "Enter · focused details    D · dispatch"
            },
            theme::hint(),
        ));
    } else {
        lines.push(Line::styled(
            "Rail Station data unavailable.",
            theme::warning(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(&title, focused))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn selected_station<'a>(
    state: &'a GameState,
    selection: &mut StationSelection,
) -> Option<&'a RailStation> {
    selection.selected_station_id(state).and_then(|station_id| {
        state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .find(|station| station.id == station_id)
    })
}

fn ready_trains(state: &GameState, station_id: RailStationId) -> Vec<&Train> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { at } if at == station_id))
        .collect()
}

fn station_name(state: &GameState, station_id: RailStationId) -> String {
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
        .and_then(|station| {
            state
                .region
                .settlements
                .iter()
                .find(|settlement| settlement.id == station.settlement_id)
        })
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| format!("Unknown Rail Station {}", station_id.get()))
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            theme::focused_border()
        } else {
            theme::border()
        })
        .title(title)
        .title_style(if focused {
            theme::focused_title()
        } else {
            theme::title()
        })
        .style(theme::panel())
}

/// Renders the current Region, Rail Network, Fleet locations, and Company Funds.
pub fn render(state: &GameState) -> String {
    render_at(state, state.last_processed_at)
}

/// Renders the map using `now` to calculate active Journey progress and ETA.
pub fn render_at(state: &GameState, now: UtcSeconds) -> String {
    let network = &state.region.rail_authority.rail_network;
    let mut output = String::new();

    writeln!(output, "Region: {}", state.region.name).expect("writing to a String cannot fail");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Legend: [S] Connected Settlement / Rail Station; [ ] Unconnected Settlement"
    )
    .expect("writing to a String cannot fail");

    writeln!(output, "\nRail Lines:").expect("writing to a String cannot fail");
    for line in &network.rail_lines {
        let first = station_label(state, line.first_station_id);
        let second = station_label(state, line.second_station_id);
        writeln!(
            output,
            "  Rail Line {}: [{first}] -- {} -- [{second}]",
            line.id.get(),
            format_distance(line.distance.metres()),
        )
        .expect("writing to a String cannot fail");
    }

    writeln!(output, "Settlements:").expect("writing to a String cannot fail");
    for settlements in state.region.settlements.chunks(4) {
        let mut first = true;
        output.push_str("  ");
        for settlement in settlements {
            if !first {
                output.push_str(" | ");
            }
            first = false;
            let marker = if network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
            {
                "[S]"
            } else {
                "[ ]"
            };
            write!(output, "{marker} {}", settlement.name)
                .expect("writing to a String cannot fail");
        }
        output.push('\n');
    }

    writeln!(output, "Trains:").expect("writing to a String cannot fail");
    if state.player_company.fleet.trains.is_empty() {
        writeln!(
            output,
            "  No Trains in the Fleet. Press B to open Buy Trains and purchase one."
        )
        .expect("writing to a String cannot fail");
    }
    for train in &state.player_company.fleet.trains {
        match train.status {
            TrainStatus::Ready { at } => {
                writeln!(
                    output,
                    "  Train {} ({}) — READY at {}",
                    train.id.get(),
                    train.model_name,
                    station_label(state, at),
                )
                .expect("writing to a String cannot fail");
            }
            TrainStatus::Travelling { journey_id } => {
                let journey = state
                    .active_journeys
                    .iter()
                    .find(|journey| journey.id == journey_id);
                match journey {
                    Some(journey) => render_travelling_train(
                        &mut output,
                        state,
                        train.id.get(),
                        &train.model_name,
                        journey,
                        now,
                    ),
                    None => writeln!(
                        output,
                        "  Train {} ({}) — TRAVELLING on Journey {} (details unavailable)",
                        train.id.get(),
                        train.model_name,
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }

    output
}

fn render_travelling_train(
    output: &mut String,
    state: &GameState,
    train_id: u64,
    model_name: &str,
    journey: &Journey,
    now: UtcSeconds,
) {
    writeln!(
        output,
        "  Train {train_id} ({model_name}) — TRAVELLING {} -> {} | progress: {}% | ETA: {}",
        station_label(state, journey.origin_station_id),
        station_label(state, journey.destination_station_id),
        journey_progress_percent(journey, now),
        format_duration(remaining_seconds(journey, now)),
    )
    .expect("writing to a String cannot fail");
}

fn station_label(state: &GameState, station_id: RailStationId) -> &str {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return "unknown Rail Station";
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map_or("unknown Settlement", |settlement| settlement.name.as_str())
}

fn journey_progress_percent(journey: &Journey, now: UtcSeconds) -> u64 {
    let duration = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds());
    if duration <= 0 {
        return 100;
    }
    let elapsed = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, duration);
    u64::try_from(elapsed.saturating_mul(100) / duration).unwrap_or(100)
}

fn remaining_seconds(journey: &Journey, now: UtcSeconds) -> u64 {
    u64::try_from(
        journey
            .arrives_at
            .unix_seconds()
            .saturating_sub(now.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

fn format_distance(metres: u64) -> String {
    let kilometres = metres / 1_000;
    let remainder = metres % 1_000;
    if remainder == 0 {
        format!("{kilometres} km")
    } else {
        format!("{kilometres}.{remainder:03} km")
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::render_at;

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    #[test]
    fn map_shows_every_settlement_and_the_seeded_rail_lines_without_colour() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let map = render_at(&state, STARTED_AT);

        for station in &state.region.rail_authority.rail_network.rail_stations {
            let settlement = state
                .region
                .settlements
                .iter()
                .find(|settlement| settlement.id == station.settlement_id)
                .unwrap();
            assert!(map.contains(&format!("[S] {}", settlement.name)));
        }
        let unconnected_count = state
            .region
            .settlements
            .iter()
            .filter(|settlement| {
                !state
                    .region
                    .rail_authority
                    .rail_network
                    .rail_stations
                    .iter()
                    .any(|station| station.settlement_id == settlement.id)
            })
            .filter(|settlement| map.contains(&format!("[ ] {}", settlement.name)))
            .count();
        assert_eq!(unconnected_count, 6);
        for settlement in &state.region.settlements {
            assert!(map.contains(&settlement.name));
        }
        for line in &state.region.rail_authority.rail_network.rail_lines {
            assert!(map.contains(&format!("Rail Line {}", line.id.get())));
        }
        assert!(map.contains("Company Funds: $"));
        assert!(map.contains("Press B to open Buy Trains"));
    }

    #[test]
    fn map_shows_ready_and_travelling_train_locations_progress_and_eta() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(2);
        let train_id = purchase_train(&mut state, 0, origin).unwrap();

        let ready_map = render_at(&state, STARTED_AT);
        assert!(ready_map.contains(&format!("Train {}", train_id.get())));
        assert!(ready_map.contains("READY at"));

        let service_id = find_or_create_service(&mut state, origin, destination).unwrap();
        let departed_at = UtcSeconds::from_unix_seconds(1_100);
        dispatch_journey(&mut state, train_id, service_id, departed_at).unwrap();
        let journey = &state.active_journeys[0];
        let halfway = UtcSeconds::from_unix_seconds(
            journey.departed_at.unix_seconds()
                + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
        );

        let travelling_map = render_at(&state, halfway);
        assert!(travelling_map.contains("TRAVELLING"));
        assert!(travelling_map.contains("progress: 50%"));
        assert!(travelling_map.contains("ETA:"));
        assert!(travelling_map.contains("Alden") || travelling_map.contains("Bellhaven"));
    }
}
