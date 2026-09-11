//! Text presentation of the Region map.
//!
//! The map reads the public Rail Network and the Player Company's operating
//! state without changing either. Markers make connection state understandable
//! in terminals where colour is unavailable.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write,
};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
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
        let [network_area, inspector_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(38)])
                .spacing(1)
                .areas(area);
        render_network_workspace(frame, network_area, state, selection);
        render_station_inspector(frame, inspector_area, state, selection, false);
    } else if details_open {
        render_station_inspector(frame, area, state, selection, true);
    } else {
        render_station_list(frame, area, state, selection, true);
    }
}

/// Stable, presentation-only coordinates for one Rail Network schematic.
///
/// Coordinates are assigned from the Rail Network graph: a highest-degree,
/// lowest-ID station roots each component, breadth-first depth sets the
/// horizontal lane, and the stable ID order separates stations vertically.
/// They intentionally describe a schematic, not geography.
#[derive(Clone, Debug, PartialEq)]
struct SchematicLayout {
    stations: Vec<SchematicStation>,
}

#[derive(Clone, Debug, PartialEq)]
struct SchematicStation {
    station_id: RailStationId,
    x: f64,
    y: f64,
    name: String,
}

fn render_network_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut StationSelection,
) {
    let station_count = state.region.rail_authority.rail_network.rail_stations.len();
    let list_height = u16::try_from(station_count)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .clamp(5, area.height.saturating_sub(13));
    let [diagram_area, list_area] =
        Layout::vertical([Constraint::Min(12), Constraint::Length(list_height)])
            .spacing(1)
            .areas(area);

    let diagram_width = diagram_area.width.saturating_sub(2);
    let diagram_height = diagram_area.height.saturating_sub(2);
    if let Some(layout) = schematic_layout(state, diagram_width, diagram_height) {
        render_schematic(frame, diagram_area, state, selection, &layout);
        render_station_list(frame, list_area, state, selection, true);
    } else {
        // Full labels are more useful than a clipped sketch. The station List
        // stays as the selection source whenever the Canvas cannot fit.
        render_station_list(frame, area, state, selection, true);
    }
}

fn render_schematic(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut StationSelection,
    layout: &SchematicLayout,
) {
    let selected_station_id = selection.selected_station_id(state);
    let network = &state.region.rail_authority.rail_network;
    let positions = layout
        .stations
        .iter()
        .map(|station| (station.station_id, station))
        .collect::<BTreeMap<_, _>>();
    let width = area.width.saturating_sub(3);
    let height = area.height.saturating_sub(3);
    let canvas = Canvas::default()
        .block(panel_block("Rail Network · schematic", true))
        .background_color(theme::PANEL)
        .marker(Marker::Braille)
        .x_bounds([0.0, f64::from(width)])
        .y_bounds([0.0, f64::from(height)])
        .paint(|context| {
            for rail_line in &network.rail_lines {
                let (Some(first), Some(second)) = (
                    positions.get(&rail_line.first_station_id),
                    positions.get(&rail_line.second_station_id),
                ) else {
                    continue;
                };
                let incident_to_selected = selected_station_id.is_some_and(|selected| {
                    rail_line.first_station_id == selected
                        || rail_line.second_station_id == selected
                });
                context.draw(&CanvasLine::new(
                    first.x,
                    first.y,
                    second.x,
                    second.y,
                    if incident_to_selected {
                        theme::ACCENT
                    } else {
                        theme::SECONDARY
                    },
                ));
            }
            context.layer();
            for station in &layout.stations {
                let is_selected = selected_station_id == Some(station.station_id);
                let marker = if is_selected { "[>]" } else { "[o]" };
                context.print(
                    station.x,
                    station.y,
                    Line::styled(
                        format!(
                            "{marker} [{:02}] {}",
                            station.station_id.get(),
                            station.name
                        ),
                        if is_selected {
                            theme::focused_title()
                        } else {
                            theme::primary_value()
                        },
                    ),
                );
            }
        });
    frame.render_widget(canvas, area);
}

fn schematic_layout(
    state: &GameState,
    canvas_width: u16,
    canvas_height: u16,
) -> Option<SchematicLayout> {
    let network = &state.region.rail_authority.rail_network;
    let station_names = network
        .rail_stations
        .iter()
        .map(|station| (station.id, station_name(state, station.id)))
        .collect::<BTreeMap<_, _>>();
    if station_names.is_empty() {
        return None;
    }

    let longest_label = station_names
        .iter()
        .map(|(station_id, name)| format!("[{:02}] {name}", station_id.get()).chars().count())
        .max()
        .unwrap_or(0);
    let longest_label = u16::try_from(longest_label).unwrap_or(u16::MAX);
    // Twenty-six cells leave a visible route field, station markers, and a
    // one-cell margin around the longest full name.
    if canvas_width < longest_label.saturating_add(26) || canvas_height < 12 {
        return None;
    }

    let mut adjacent = station_names
        .keys()
        .copied()
        .map(|station_id| (station_id, Vec::new()))
        .collect::<BTreeMap<_, Vec<_>>>();
    for rail_line in &network.rail_lines {
        if adjacent.contains_key(&rail_line.first_station_id)
            && adjacent.contains_key(&rail_line.second_station_id)
        {
            adjacent
                .entry(rail_line.first_station_id)
                .or_default()
                .push(rail_line.second_station_id);
            adjacent
                .entry(rail_line.second_station_id)
                .or_default()
                .push(rail_line.first_station_id);
        }
    }
    for neighbours in adjacent.values_mut() {
        neighbours.sort_unstable();
        neighbours.dedup();
    }

    let mut unplaced = station_names.keys().copied().collect::<BTreeSet<_>>();
    let mut depths = BTreeMap::new();
    let mut component = 0_usize;
    while !unplaced.is_empty() {
        let root = unplaced.iter().copied().min_by_key(|station_id| {
            (
                Reverse(adjacent.get(station_id).map_or(0, Vec::len)),
                *station_id,
            )
        })?;
        let mut queue = VecDeque::from([(root, 0_usize)]);
        unplaced.remove(&root);
        while let Some((station_id, depth)) = queue.pop_front() {
            depths.insert(station_id, (component, depth));
            for neighbour in adjacent.get(&station_id).into_iter().flatten() {
                if unplaced.remove(neighbour) {
                    queue.push_back((*neighbour, depth.saturating_add(1)));
                }
            }
        }
        component = component.saturating_add(1);
    }

    let maximum_depth = depths.values().map(|(_, depth)| *depth).max().unwrap_or(0);
    let mut rows_by_depth = BTreeMap::<usize, Vec<RailStationId>>::new();
    for (&station_id, &(_, depth)) in &depths {
        rows_by_depth.entry(depth).or_default().push(station_id);
    }
    let label_start_limit = canvas_width.saturating_sub(longest_label.saturating_add(3));
    let route_width = label_start_limit.saturating_sub(2);
    let bottom = canvas_height.saturating_sub(2);
    let vertical_span = bottom.saturating_sub(1);
    let mut coordinates = BTreeMap::new();
    for (depth, stations) in rows_by_depth {
        let x = if maximum_depth == 0 {
            label_start_limit / 2
        } else {
            1_u16.saturating_add(
                route_width.saturating_mul(u16::try_from(depth).unwrap_or(u16::MAX))
                    / u16::try_from(maximum_depth).unwrap_or(1),
            )
        };
        let station_count = stations.len().saturating_sub(1);
        for (index, station_id) in stations.into_iter().enumerate() {
            let y = if station_count == 0 {
                1_u16.saturating_add(vertical_span / 2)
            } else {
                1_u16.saturating_add(
                    vertical_span.saturating_mul(u16::try_from(index).unwrap_or(u16::MAX))
                        / u16::try_from(station_count).unwrap_or(1),
                )
            };
            coordinates.insert(station_id, (f64::from(x), f64::from(y)));
        }
    }

    Some(SchematicLayout {
        stations: station_names
            .into_iter()
            .filter_map(|(station_id, name)| {
                coordinates
                    .get(&station_id)
                    .map(|&(x, y)| SchematicStation {
                        station_id,
                        x,
                        y,
                        name,
                    })
            })
            .collect(),
    })
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
        let incident_lines = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .filter(|rail_line| {
                rail_line.first_station_id == station.id
                    || rail_line.second_station_id == station.id
            })
            .collect::<Vec<_>>();
        lines.push(Line::styled(
            format!("Incident Rail Lines ({})", incident_lines.len()),
            theme::title(),
        ));
        if incident_lines.is_empty() {
            lines.push(Line::styled(
                "No Rail Lines are recorded for this Rail Station.",
                theme::warning(),
            ));
        } else {
            lines.extend(incident_lines.into_iter().map(|rail_line| {
                let first = station_name(state, rail_line.first_station_id);
                let second = station_name(state, rail_line.second_station_id);
                Line::from(vec![
                    Span::styled(
                        format!("Rail Line {:02}  ", rail_line.id.get()),
                        theme::secondary(),
                    ),
                    Span::raw(format!(
                        "{first} → {second} · {}",
                        format_distance(rail_line.distance.metres())
                    )),
                ])
            }));
        }
        lines.push(Line::from(""));
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

    use super::{render_at, schematic_layout};

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

    #[test]
    fn schematic_layout_is_stable_and_separates_the_actual_branching_edges() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let first = schematic_layout(&state, 72, 20).expect("starter labels fit the Canvas");
        let second = schematic_layout(&state, 72, 20).expect("starter labels fit the Canvas");
        assert_eq!(first, second, "placement must not depend on render order");

        let positions = first
            .stations
            .iter()
            .map(|station| (station.station_id, (station.x, station.y)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let network = &state.region.rail_authority.rail_network;
        for rail_line in &network.rail_lines {
            assert!(positions.contains_key(&rail_line.first_station_id));
            assert!(positions.contains_key(&rail_line.second_station_id));
        }

        let branch = positions
            .get(&RailStationId::new(2))
            .expect("the starter topology branches at Rail Station 02");
        let branch_neighbours = [
            RailStationId::new(1),
            RailStationId::new(3),
            RailStationId::new(4),
        ]
        .into_iter()
        .map(|station_id| positions.get(&station_id).expect("branch neighbour exists"))
        .collect::<Vec<_>>();
        assert!(branch_neighbours.iter().all(|(x, _)| x > &branch.0));
        assert_ne!(branch_neighbours[0].1, branch_neighbours[1].1);
        assert_ne!(branch_neighbours[1].1, branch_neighbours[2].1);
    }
}
