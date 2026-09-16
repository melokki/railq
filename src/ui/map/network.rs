//! Rail Network, Rail Station, Settlement, and Journey browsing for the Map workspace.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, VecDeque},
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

use super::shared::{format_distance, train_model_name};
use super::operational::market_maturity_percent;
use super::journeys::{
    JourneySelection, render_compact_journey_board, render_journey_inspector,
    render_journey_workspace,
};
use crate::{
    model::{GameState, RailStation, RailStationId, Settlement, SettlementId, Train, TrainStatus, UtcSeconds},
    sim::demand::effective_arrival_rate_per_hour,
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

/// The secondary dataset visible in the Map workspace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MapFocus {
    #[default]
    Stations,
    Settlements,
    Journeys,
}

impl MapFocus {
    pub const fn is_stations(self) -> bool {
        matches!(self, Self::Stations)
    }

    pub const fn is_journeys(self) -> bool {
        matches!(self, Self::Journeys)
    }
}

/// The three independent Map browse selections supplied to one render pass.
/// Grouping them keeps the dashboard interface focused as more Map panels are
/// added without coupling presentation state to the simulation.
pub struct MapSelections<'a> {
    pub stations: &'a mut StationSelection,
    pub settlements: &'a mut SettlementSelection,
    pub journeys: &'a mut JourneySelection,
}

/// Presentation-only selection for Settlements without a Rail Station.
///
/// The filtered list is rebuilt from the Region on every interaction, while
/// the selected stable ID keeps the user's place through redraws and resize.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettlementSelection {
    selected_settlement_id: Option<SettlementId>,
    list_state: ListState,
    page_size: usize,
}

impl SettlementSelection {
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let settlements = unconnected_settlements(state);
        let Some(selected) = self.list_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let last = settlements.len().saturating_sub(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected.saturating_add(1).min(last),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected.saturating_add(page_size).min(last),
            _ => selected,
        };
        self.select_index(&settlements, next);
    }

    pub fn selected_settlement_id(&mut self, state: &GameState) -> Option<SettlementId> {
        self.synchronize(state);
        self.selected_settlement_id
    }

    fn synchronize(&mut self, state: &GameState) {
        let settlements = unconnected_settlements(state);
        let previous_index = self.list_state.selected().unwrap_or(0);
        let selected = self
            .selected_settlement_id
            .and_then(|settlement_id| {
                settlements
                    .iter()
                    .position(|settlement| settlement.id == settlement_id)
            })
            .or_else(|| {
                (!settlements.is_empty()).then_some(previous_index.min(settlements.len() - 1))
            });
        if let Some(index) = selected {
            self.selected_settlement_id = Some(settlements[index].id);
        } else {
            self.selected_settlement_id = None;
            *self.list_state.offset_mut() = 0;
        }
        self.list_state.select(selected);
    }

    fn select_index(&mut self, settlements: &[&Settlement], index: usize) {
        let Some(settlement) = settlements.get(index) else {
            return;
        };
        self.selected_settlement_id = Some(settlement.id);
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
    now: UtcSeconds,
    selections: MapSelections<'_>,
    focus: MapFocus,
    details_open: bool,
) {
    let MapSelections {
        stations: selection,
        settlements: settlement_selection,
        journeys: journey_selection,
    } = selections;
    match focus {
        MapFocus::Stations => selection.synchronize(state),
        MapFocus::Settlements => settlement_selection.synchronize(state),
        MapFocus::Journeys => journey_selection.synchronize(state),
    }
    if focus == MapFocus::Journeys {
        if area.width >= 96 && area.height >= 14 {
            render_journey_workspace(frame, area, state, now, journey_selection);
        } else if details_open {
            render_journey_inspector(frame, area, state, now, journey_selection, true);
        } else {
            render_compact_journey_board(frame, area, state, now, journey_selection);
        }
    } else if !focus.is_stations() {
        if area.width >= 96 && area.height >= 14 {
            render_settlement_workspace(frame, area, state, settlement_selection);
        } else if details_open {
            render_settlement_inspector(frame, area, state, settlement_selection, true);
        } else {
            render_settlement_list(frame, area, state, settlement_selection, true);
        }
    } else if area.width >= 96 && area.height >= 14 {
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

fn render_settlement_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut SettlementSelection,
) {
    let [list_area, inspector_area] =
        Layout::horizontal([Constraint::Min(42), Constraint::Length(38)])
            .spacing(1)
            .areas(area);
    render_settlement_list(frame, list_area, state, selection, true);
    render_settlement_inspector(frame, inspector_area, state, selection, false);
}

/// Stable, presentation-only coordinates for one Rail Network schematic.
///
/// Coordinates are assigned from the Rail Network graph: a highest-degree,
/// lowest-ID station roots each component, breadth-first depth sets the
/// horizontal lane, and the stable ID order separates stations vertically.
/// They intentionally describe a schematic, not geography.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SchematicLayout {
    pub(super) stations: Vec<SchematicStation>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SchematicStation {
    pub(super) station_id: RailStationId,
    pub(super) x: f64,
    pub(super) y: f64,
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
    let registration_title = format!(
        "Rail Network · {} {}",
        state.region.railway_registration.display_code(),
        state.region.railway_registration.mark
    );
    let canvas = Canvas::default()
        .block(panel_block(&registration_title, true))
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

pub(super) fn schematic_layout(
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
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, &mut selection.list_state);
}

fn render_settlement_list(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut SettlementSelection,
    focused: bool,
) {
    let visible_items = usize::from(area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);
    selection.keep_compact_selection_visible(visible_items);
    let items = unconnected_settlements(state)
        .into_iter()
        .map(|settlement| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{:02}] ", settlement.id.get()), theme::secondary()),
                Span::raw(settlement.name.clone()),
                Span::styled(
                    format!(
                        "  POP {}  UNCONNECTED",
                        format_population(settlement.population)
                    ),
                    theme::secondary(),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block(
            "Unconnected Settlements · inspect-only",
            focused,
        ))
        .highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, &mut selection.list_state);
}

fn render_settlement_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut SettlementSelection,
    focused: bool,
) {
    let selected = selected_settlement(state, selection);
    let title = selected.map_or_else(
        || "Selected Settlement".to_owned(),
        |settlement| {
            format!(
                "Settlement {:02} · {}",
                settlement.id.get(),
                settlement.name
            )
        },
    );
    let mut lines = Vec::new();
    if let Some(settlement) = selected {
        lines.push(Line::styled("Connection status", theme::title()));
        lines.push(Line::from(vec![
            Span::styled("UNCONNECTED  ", theme::warning()),
            Span::styled("No Rail Station", theme::secondary()),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::styled("Population", theme::title()));
        lines.push(Line::styled(
            format_population(settlement.population),
            theme::primary_value(),
        ));
        lines.push(Line::from(""));
        lines.push(Line::styled("Operations", theme::title()));
        lines.push(Line::styled(
            "Inspection only — no dispatch, construction, or demand data.",
            theme::secondary(),
        ));
        lines.push(Line::from(""));
        lines.push(Line::styled(
            if focused {
                "Esc · Settlements    Tab · Rail Stations"
            } else {
                "Enter · focused details    Tab · Rail Stations"
            },
            theme::hint(),
        ));
    } else {
        lines.push(Line::styled(
            "No unconnected Settlements are recorded.",
            theme::secondary(),
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
                    Span::raw(train_model_name(train)),
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
                        "{} waiting · +{}/h · {} adoption",
                        pool.waiting_passengers,
                        effective_arrival_rate_per_hour(state, pool),
                        market_maturity_percent(pool.market_maturity.basis_points())
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

fn selected_settlement<'a>(
    state: &'a GameState,
    selection: &mut SettlementSelection,
) -> Option<&'a Settlement> {
    selection
        .selected_settlement_id(state)
        .and_then(|settlement_id| {
            unconnected_settlements(state)
                .into_iter()
                .find(|settlement| settlement.id == settlement_id)
        })
}

fn unconnected_settlements(state: &GameState) -> Vec<&Settlement> {
    let connected = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.settlement_id)
        .collect::<BTreeSet<_>>();
    state
        .region
        .settlements
        .iter()
        .filter(|settlement| !connected.contains(&settlement.id))
        .collect()
}

pub(super) fn format_population(population: u64) -> String {
    let digits = population.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

pub(super) fn ready_trains(state: &GameState, station_id: RailStationId) -> Vec<&Train> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { at } if at == station_id))
        .collect()
}

pub(super) fn station_name(state: &GameState, station_id: RailStationId) -> String {
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

pub(super) fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
}

pub(super) fn panel_block(title: &str, focused: bool) -> Block<'_> {
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
