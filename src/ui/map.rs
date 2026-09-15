//! Text presentation of the Region map.
//!
//! The map reads the public Rail Network and the Player Company's operating
//! state without changing either. Markers make connection state understandable
//! in terminals where colour is unavailable.

use std::fmt::Write;

use crossterm::event::KeyCode;
use ratatui::{Frame, layout::Rect};

mod geometry;
mod journeys;
mod network;
mod operational;

pub use journeys::JourneySelection;
pub use network::{MapFocus, MapSelections, SettlementSelection, StationSelection, render_dashboard};
#[cfg(test)]
use network::schematic_layout;
use operational::{
    MapDirection, journey_next_stop_station_id, operational_layout, render_operational_map,
};
#[cfg(test)]
use operational::{
    focus_rank, journey_route_segments, map_place_label,
    place_link_distance_label, point_along_orthogonal_rail, selected_neighbours,
};

use crate::{
    catalog::model_for_train,
    model::{
        GameState, Journey, Money, RailStationId, SettlementId, Train,
        TrainStatus, UtcSeconds,
    },
};

/// One keyboard selection across every Settlement shown by the operational map.
///
/// Connected Settlements resolve to a Rail Station and can start Manual Dispatch;
/// unconnected Settlements remain inspectable without pretending they already
/// have railway infrastructure.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapLocationSelection {
    selected_settlement_id: Option<SettlementId>,
}

/// Presentation state and input ownership for the primary operational Map.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapWorkspace {
    location_selection: MapLocationSelection,
    world_details_visible: bool,
}

/// Intent emitted by Map input which must be handled by the outer Shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapWorkspaceAction {
    Continue,
    ClearNotice,
    OpenServices,
    StartDispatch,
}

/// Result of routing a key while the World Details overlay owns focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldDetailsKeyAction {
    Continue,
    Closed,
    ClosedForNavigation,
}

/// One contextual footer action owned by the Map workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl MapShortcut {
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

impl MapWorkspace {
    /// Routes input owned by the operational Map.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) -> MapWorkspaceAction {
        match key {
            KeyCode::Char('w' | 'W') => {
                self.world_details_visible = true;
                MapWorkspaceAction::ClearNotice
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('h' | 'H' | 'j' | 'J' | 'k' | 'K' | 'l' | 'L') => {
                self.location_selection.handle_key(key, state);
                MapWorkspaceAction::ClearNotice
            }
            KeyCode::Char('s' | 'S') => MapWorkspaceAction::OpenServices,
            KeyCode::Char('d' | 'D') => MapWorkspaceAction::StartDispatch,
            _ => MapWorkspaceAction::Continue,
        }
    }

    /// Routes input while World Details is the focused informational overlay.
    pub fn handle_world_details_key(&mut self, key: KeyCode) -> WorldDetailsKeyAction {
        match key {
            KeyCode::Esc | KeyCode::Char('w' | 'W') => {
                self.world_details_visible = false;
                WorldDetailsKeyAction::Closed
            }
            KeyCode::Char(
                '1' | '2' | '3' | '4' | '5' | '6' | 'm' | 'M' | 't' | 'T' | 'b' | 'B'
                | 'c' | 'C' | 'a' | 'A' | 'u' | 'U',
            ) => {
                self.world_details_visible = false;
                WorldDetailsKeyAction::ClosedForNavigation
            }
            _ => WorldDetailsKeyAction::Continue,
        }
    }

    /// Returns whether the World Details overlay currently owns focus.
    pub fn world_details_visible(&self) -> bool {
        self.world_details_visible
    }

    /// Returns the contextual controls owned by the Map workspace.
    pub fn shortcuts(&self, state: &GameState, compact: bool) -> Vec<MapShortcut> {
        if self.world_details_visible {
            return vec![MapShortcut::enabled("W/Esc", "Close")];
        }

        let train_count = state.player_company.fleet.trains.len();
        let ready = state
            .player_company
            .fleet
            .trains
            .iter()
            .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
            .count();

        let mut items = vec![MapShortcut::enabled(
            if compact { "↑↓←→" } else { "↑↓←→/HJKL" },
            "Station",
        )];
        if train_count == 0 {
            items.push(MapShortcut::enabled("3", "Market"));
        } else if ready == 0 {
            items.push(MapShortcut::disabled("D", "Dispatch"));
        } else {
            items.push(MapShortcut::enabled("D", "Dispatch"));
        }
        items.push(MapShortcut::enabled("S", "Services"));
        items.push(MapShortcut::enabled("W", "World"));
        items
    }

    /// Returns help text for the currently focused Map surface.
    pub fn help_lines(&self, state: &GameState) -> Vec<String> {
        if self.world_details_visible {
            return vec![
                "Current · World Details".into(),
                "w / Esc Return to Map".into(),
                "The railway registration belongs to the Region and remains stable for this save."
                    .into(),
            ];
        }

        let train_count = state.player_company.fleet.trains.len();
        let ready = state
            .player_company
            .fleet
            .trains
            .iter()
            .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
            .count();
        let mut lines = vec![
            "Current · Map".into(),
            "↑↓←→ / hjkl Select a map location".into(),
            "s Open Passenger Services".into(),
            "w Open World Details".into(),
        ];
        if train_count == 0 {
            lines.extend([
                String::new(),
                "Next step".into(),
                "3 Open Market and acquire your first passenger Train".into(),
            ]);
        } else if ready == 0 {
            lines.extend([
                "d Dispatch is unavailable while every Train is travelling".into(),
                "Journeys continue while RailQ is closed; dispatch again after arrival".into(),
            ]);
        } else {
            lines.push(format!(
                "d Manual Dispatch · {ready} READY {}",
                if ready == 1 { "Train" } else { "Trains" }
            ));
        }
        lines
    }

    /// Renders the operational Map with its stable location selection.
    pub fn render_dashboard(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_operational_map(frame, area, state, &mut self.location_selection);
    }

    /// Clears transient Map presentation state for a fresh game.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl MapLocationSelection {
    /// Returns the selected Settlement after reconciling a stale selection.
    pub fn selected_settlement_id(&mut self, state: &GameState) -> Option<SettlementId> {
        self.synchronize(state);
        self.selected_settlement_id
    }

    /// Resolves the selected Settlement to its Rail Station when connected.
    pub fn selected_station_id(&mut self, state: &GameState) -> Option<RailStationId> {
        let settlement_id = self.selected_settlement_id(state)?;
        state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .find(|station| station.settlement_id == settlement_id)
            .map(|station| station.id)
    }

    /// Moves spatially through the same logical layout used by the map.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let Some(current_id) = self.selected_settlement_id else {
            return;
        };
        let Some(layout) = operational_layout(state) else {
            return;
        };
        let Some(current) = layout
            .places
            .iter()
            .find(|place| place.settlement_id == current_id)
        else {
            return;
        };

        let direction = match key {
            KeyCode::Left | KeyCode::Char('h' | 'H') => Some(MapDirection::Left),
            KeyCode::Right | KeyCode::Char('l' | 'L') => Some(MapDirection::Right),
            KeyCode::Up | KeyCode::Char('k' | 'K') => Some(MapDirection::Up),
            KeyCode::Down | KeyCode::Char('j' | 'J') => Some(MapDirection::Down),
            _ => None,
        };
        let Some(direction) = direction else {
            return;
        };

        let next = layout
            .places
            .iter()
            .filter(|candidate| candidate.settlement_id != current.settlement_id)
            .filter_map(|candidate| {
                let dx = candidate.x - current.x;
                let dy = candidate.y - current.y;
                let (primary, secondary) = match direction {
                    MapDirection::Left if dx < 0 => (-dx, dy.abs()),
                    MapDirection::Right if dx > 0 => (dx, dy.abs()),
                    MapDirection::Up if dy < 0 => (-dy, dx.abs()),
                    MapDirection::Down if dy > 0 => (dy, dx.abs()),
                    _ => return None,
                };
                // Prefer the nearest place in the requested direction while
                // strongly penalising a large perpendicular jump.
                Some((
                    primary.saturating_mul(10) + secondary,
                    candidate.settlement_id,
                ))
            })
            .min_by_key(|(score, settlement_id)| (*score, *settlement_id));

        if let Some((_, settlement_id)) = next {
            self.selected_settlement_id = Some(settlement_id);
        }
    }

    fn synchronize(&mut self, state: &GameState) {
        if self.selected_settlement_id.is_some_and(|selected| {
            state
                .region
                .settlements
                .iter()
                .any(|settlement| settlement.id == selected)
        }) {
            return;
        }

        self.selected_settlement_id = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .first()
            .map(|station| station.settlement_id)
            .or_else(|| {
                state
                    .region
                    .settlements
                    .first()
                    .map(|settlement| settlement.id)
            });
    }
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
        "Rail registration: {} · {}",
        state.region.railway_registration.display_code(),
        state.region.railway_registration.mark
    )
    .expect("writing to a String cannot fail");
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
            "  No Trains in the Fleet. Open 3 Market to purchase one."
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
                    train_model_name(train),
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
                        &train_model_name(train),
                        journey,
                        now,
                    ),
                    None => writeln!(
                        output,
                        "  Train {} ({}) — TRAVELLING on Journey {} (details unavailable)",
                        train.id.get(),
                        train_model_name(train),
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }

    output
}

fn train_model_name(train: &Train) -> String {
    let model = model_for_train(train)
        .map(|model| model.name().to_owned())
        .unwrap_or_else(|| format!("Unknown model ({})", train.model_id.as_str()));
    train
        .nickname
        .as_ref()
        .map(|nickname| format!("{} · {model}", nickname.as_str()))
        .unwrap_or(model)
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
        "  Train {train_id} ({model_name}) — TRAVELLING {} -> {} | next: {} | leg: {}% | ETA: {} | onboard: {}",
        station_label(state, journey.origin_station_id),
        station_label(state, journey.destination_station_id),
        journey_next_stop_station_id(state, journey)
            .map(|station_id| station_label(state, station_id))
            .unwrap_or("unknown Rail Station"),
        journey_progress_percent(journey, now),
        format_duration(remaining_seconds(journey, now)),
        journey.onboard_passengers(),
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
    crate::ui::format::money(money)
}

fn format_distance(metres: u64) -> String {
    crate::ui::format::distance(metres)
}

fn format_duration(seconds: u64) -> String {
    crate::ui::format::duration(seconds)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{
        MapDirection, MapWorkspace, MapWorkspaceAction, WorldDetailsKeyAction, focus_rank,
        journey_progress_percent,
        journey_route_segments, map_place_label, operational_layout, place_link_distance_label,
        point_along_orthogonal_rail, render_at, schematic_layout, selected_neighbours,
    };
    use super::geometry::{MapCell, RAIL_LEFT, RAIL_RIGHT, rail_glyph};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    #[test]
    fn workspace_owns_primary_map_actions() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MapWorkspace::default();

        assert_eq!(
            workspace.handle_key(KeyCode::Char('s'), &state),
            MapWorkspaceAction::OpenServices
        );
        assert_eq!(
            workspace.handle_key(KeyCode::Char('d'), &state),
            MapWorkspaceAction::StartDispatch
        );
        assert_eq!(
            workspace.handle_key(KeyCode::Right, &state),
            MapWorkspaceAction::ClearNotice
        );
    }

    #[test]
    fn workspace_owns_world_details_focus() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MapWorkspace::default();

        assert_eq!(
            workspace.handle_key(KeyCode::Char('w'), &state),
            MapWorkspaceAction::ClearNotice
        );
        assert!(workspace.world_details_visible());
        assert_eq!(
            workspace.handle_world_details_key(KeyCode::Char('2')),
            WorldDetailsKeyAction::ClosedForNavigation
        );
        assert!(!workspace.world_details_visible());
    }

    #[test]
    fn workspace_owns_map_controls_and_help() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MapWorkspace::default();

        let shortcuts = workspace.shortcuts(&state, false);
        assert!(shortcuts.iter().any(|shortcut| shortcut.key == "3"));
        assert!(shortcuts.iter().any(|shortcut| shortcut.key == "W"));
        assert!(workspace.help_lines(&state)[0].contains("Map"));

        workspace.handle_key(KeyCode::Char('w'), &state);
        let modal_shortcuts = workspace.shortcuts(&state, false);
        assert_eq!(modal_shortcuts.len(), 1);
        assert_eq!(modal_shortcuts[0].key, "W/Esc");
        assert!(workspace.help_lines(&state)[0].contains("World Details"));
    }

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
        assert!(map.contains("Open 3 Market"));
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
    fn operational_map_labels_keep_selection_strong_without_repeating_counts() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let station_id = RailStationId::new(1);
        purchase_train(&mut state, 0, station_id).unwrap();
        let layout = operational_layout(&state).expect("starter map layout");
        let place = layout
            .places
            .iter()
            .find(|place| place.station_id == Some(station_id))
            .expect("starter Rail Station should be on map");

        let normal = map_place_label(place, None);
        assert_eq!(normal, place.name);

        let selected = map_place_label(place, Some(place.settlement_id));
        assert_eq!(selected, place.name.to_uppercase());
    }

    #[test]
    fn focus_priority_gives_selection_and_direct_neighbours_first_claim_on_labels() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let layout = operational_layout(&state).expect("starter map layout");
        let selected = layout
            .places
            .iter()
            .find(|place| place.station_id == Some(RailStationId::new(1)))
            .expect("starter station 01 should be on map");
        let adjacent = selected_neighbours(&layout, Some(selected.settlement_id));

        assert_eq!(
            focus_rank(selected, Some(selected.settlement_id), &adjacent),
            0
        );

        let neighbour = layout
            .places
            .iter()
            .find(|place| adjacent.contains(&place.settlement_id))
            .expect("starter station 01 has a direct neighbour");
        assert_eq!(
            focus_rank(neighbour, Some(selected.settlement_id), &adjacent),
            1
        );

        let background_station = layout
            .places
            .iter()
            .find(|place| {
                place.station_id.is_some()
                    && place.settlement_id != selected.settlement_id
                    && !adjacent.contains(&place.settlement_id)
            })
            .expect("starter network has a non-adjacent connected station");
        assert_eq!(
            focus_rank(background_station, Some(selected.settlement_id), &adjacent),
            2
        );

        let unconnected = layout
            .places
            .iter()
            .find(|place| place.station_id.is_none())
            .expect("starter world has an unconnected settlement");
        assert_eq!(
            focus_rank(unconnected, Some(selected.settlement_id), &adjacent),
            3
        );
    }

    #[test]
    fn selected_routes_use_heavy_rail_while_background_routes_stay_light() {
        let horizontal = RAIL_LEFT | RAIL_RIGHT;
        assert_eq!(rail_glyph(horizontal, false, false), '─');
        assert_eq!(rail_glyph(horizontal, true, false), '━');
        assert_eq!(rail_glyph(horizontal, false, true), '═');
    }

    #[test]
    fn selected_link_distance_label_is_drawn_off_the_rail_when_space_allows() {
        let mut grid = vec![vec![MapCell::default(); 32]; 8];
        place_link_distance_label(&mut grid, (3, 4), (24, 4), 42_000);
        let rendered = grid
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("42 km"));
    }

    #[test]
    fn train_marker_follows_horizontal_and_vertical_rail_direction() {
        assert_eq!(
            point_along_orthogonal_rail((0, 0), (10, 0), 0.5),
            ((5, 0), '▶')
        );
        assert_eq!(
            point_along_orthogonal_rail((10, 0), (0, 0), 0.5),
            ((5, 0), '◀')
        );
        assert_eq!(
            point_along_orthogonal_rail((0, 0), (0, 6), 0.5),
            ((0, 3), '▼')
        );
        assert_eq!(
            point_along_orthogonal_rail((0, 6), (0, 0), 0.5),
            ((0, 3), '▲')
        );
    }

    #[test]
    fn active_journey_reuses_the_service_rail_line_path_for_map_movement() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(3);
        let train_id = purchase_train(&mut state, 0, origin).unwrap();
        let service_id = find_or_create_service(&mut state, origin, destination).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();

        let journey = &state.active_journeys[0];
        let segments = journey_route_segments(&state, journey).expect("service path should render");

        assert_eq!(segments.first().unwrap().from_station_id, origin);
        assert_eq!(segments.last().unwrap().to_station_id, destination);
        assert_eq!(
            segments.len(),
            state.player_company.passenger_services[0]
                .rail_line_ids
                .len()
        );
    }

    #[test]
    fn journey_progress_is_clamped_before_departure_and_after_arrival() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let journey = &state.active_journeys[0];

        assert_eq!(
            journey_progress_percent(
                journey,
                UtcSeconds::from_unix_seconds(journey.departed_at.unix_seconds() - 1)
            ),
            0
        );
        assert_eq!(
            journey_progress_percent(
                journey,
                UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() + 1)
            ),
            100
        );
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
