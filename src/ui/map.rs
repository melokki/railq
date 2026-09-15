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
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, LineGauge, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::{
    catalog::model_for_train,
    model::{
        GameState, Journey, JourneyId, Money, RailStation, RailStationId, Settlement, SettlementId,
        Train, TrainId, TrainStatus, UtcSeconds,
    },
    sim::{
        authority::{
            COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS, local_rail_success_basis_points,
            nearest_connection_station_id,
        },
        demand::effective_arrival_rate_per_hour,
        services::path_between_stations,
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

/// One keyboard selection across every Settlement shown by the operational map.
///
/// Connected Settlements resolve to a Rail Station and can start Manual Dispatch;
/// unconnected Settlements remain inspectable without pretending they already
/// have railway infrastructure.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapLocationSelection {
    selected_settlement_id: Option<SettlementId>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MapDirection {
    Left,
    Right,
    Up,
    Down,
}

// Most terminal cells are roughly twice as tall as they are wide. Keep this
// isolated so the visual calibration can be tuned later without touching the
// distance curve itself.
const TERMINAL_CELL_HEIGHT_TO_WIDTH: i32 = 2;

#[derive(Clone, Debug)]
struct OperationalLayout {
    places: Vec<OperationalPlace>,
    lines: Vec<OperationalLine>,
}

#[derive(Clone, Debug)]
struct OperationalPlace {
    settlement_id: SettlementId,
    station_id: Option<RailStationId>,
    name: String,
    x: i32,
    y: i32,
}

#[derive(Clone, Copy, Debug)]
struct OperationalLine {
    first_settlement_id: SettlementId,
    second_settlement_id: SettlementId,
    distance_metres: u64,
    track_count: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MapInk {
    Empty,
    Rail,
    RailAccent,
    RailLabel,
    Connected,
    ConnectedAdjacent,
    Unconnected,
    Selected,
    Train,
}

#[derive(Clone, Copy, Debug)]
struct MapCell {
    ch: char,
    ink: MapInk,
}

impl Default for MapCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            ink: MapInk::Empty,
        }
    }
}

const RAIL_LEFT: u8 = 1;
const RAIL_RIGHT: u8 = 2;
const RAIL_UP: u8 = 4;
const RAIL_DOWN: u8 = 8;

/// Renders one persistent operational map containing both connected and
/// unconnected Settlements. Wide terminals keep a contextual inspector beside
/// the network; compact terminals stack it underneath without replacing the map.
pub fn render_operational_map(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    selection.synchronize(state);
    if area.width >= 92 && area.height >= 14 {
        let inspector_width = if area.width >= 120 { 40 } else { 36 };
        let [map_area, inspector_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(inspector_width)])
                .spacing(1)
                .areas(area);
        render_operational_network(frame, map_area, state, selection);
        render_location_inspector(frame, inspector_area, state, selection);
    } else if area.height >= 17 {
        let inspector_height = area.height.min(10);
        let [map_area, inspector_area] =
            Layout::vertical([Constraint::Min(7), Constraint::Length(inspector_height)])
                .spacing(1)
                .areas(area);
        render_operational_network(frame, map_area, state, selection);
        render_location_inspector(frame, inspector_area, state, selection);
    } else {
        render_operational_network(frame, area, state, selection);
    }
}

fn render_operational_network(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    let selected = selection.selected_settlement_id(state);
    let block = operational_network_block(state, area.width);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(layout) = operational_layout(state) else {
        frame.render_widget(
            Paragraph::new("No Settlements are available.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };
    if inner.width < 8 || inner.height < 4 {
        return;
    }

    let rows = render_map_rows(&layout, selected, inner.width, inner.height, state);
    frame.render_widget(
        Paragraph::new(rows)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn operational_network_block(state: &GameState, width: u16) -> Block<'static> {
    let registration = format!(
        "{} {}",
        state.region.railway_registration.display_code(),
        state.region.railway_registration.mark
    );

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::focused_border())
        .title_top(Line::styled(" Network ", theme::focused_title()).left_aligned())
        .style(theme::panel());

    // Keep identity separate from the workspace name. The previous single title
    // mixed registration and map-marker explanations into one long sentence,
    // which made the panel harder to scan than the map itself.
    if width >= 40 {
        block = block.title_top(
            Line::from(vec![
                Span::styled(" Registration · ", theme::secondary()),
                Span::styled(format!("{registration} "), theme::primary_value()),
            ])
            .right_aligned(),
        );
    }

    // Marker meanings are spatial information, so the map keeps only this
    // compact legend. The selected location is already identified by its map
    // marker/label and by the inspector when one is visible.
    if width >= 82 {
        block = block.title_bottom(
            Line::from(vec![
                Span::styled(" ● ", theme::primary_value()),
                Span::styled("station", theme::secondary()),
                Span::styled("   ○ ", theme::secondary()),
                Span::styled("settlement", theme::secondary()),
                Span::styled("   ─ ", theme::secondary()),
                Span::styled("single", theme::secondary()),
                Span::styled("   ═ ", theme::secondary()),
                Span::styled("double", theme::secondary()),
                Span::styled("   ▶ ", theme::warning()),
                Span::styled("train ", theme::secondary()),
            ])
            .right_aligned(),
        );
    }

    block
}

fn render_location_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut MapLocationSelection,
) {
    let selected_id = selection.selected_settlement_id(state);
    let settlement = selected_id.and_then(|selected_id| {
        state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == selected_id)
    });
    let Some(settlement) = settlement else {
        frame.render_widget(
            Paragraph::new("No Settlement selected.")
                .block(panel_block("Inspector", false))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let station = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.settlement_id == settlement.id);
    let compact = area.height < 18;

    // The inspector owns facts about the selected location. Connectivity and
    // route geometry stay on the map; keyboard actions stay in the footer.
    let lines = if let Some(station) = station {
        let ready_count = ready_trains(state, station.id).len();
        let service_count = state
            .player_company
            .passenger_services
            .iter()
            .filter(|service| service.stop_station_ids.contains(&station.id))
            .count();

        let mut demand = state
            .origin_destination_demand
            .iter()
            .filter(|pool| pool.origin_station_id == station.id)
            .map(|pool| {
                (
                    station_name(state, pool.destination_station_id),
                    pool.waiting_passengers,
                    effective_arrival_rate_per_hour(state, pool),
                    pool.market_maturity.basis_points(),
                )
            })
            .collect::<Vec<_>>();
        demand.sort_by_key(|(_, waiting, _, _)| Reverse(*waiting));
        let waiting_total = demand.iter().fold(0_u32, |total, (_, waiting, _, _)| {
            total.saturating_add(*waiting)
        });
        let arrival_rate_total = demand.iter().fold(0_u32, |total, (_, _, per_hour, _)| {
            total.saturating_add(*per_hour)
        });
        let average_maturity_basis_points = if demand.is_empty() {
            0
        } else {
            let total = demand.iter().fold(0_u64, |total, (_, _, _, maturity)| {
                total.saturating_add(u64::from(*maturity))
            });
            (total / demand.len() as u64) as u16
        };

        let mut lines = vec![
            inspector_metric("Station", &format!("{:02}", station.id.get())),
            inspector_metric("Population", &format_population(settlement.population)),
        ];

        if compact {
            lines.push(inspector_metric("Ready here", &ready_count.to_string()));
            lines.push(inspector_metric("Services", &service_count.to_string()));
            lines.push(inspector_metric(
                "Rail adoption",
                &market_maturity_summary(average_maturity_basis_points),
            ));
            lines.push(inspector_metric(
                "Demand",
                &format!(
                    "{} · +{arrival_rate_total}/h",
                    format_population(u64::from(waiting_total))
                ),
            ));
        } else {
            lines.push(Line::from(""));
            lines.push(inspector_section("OPERATIONS"));
            lines.push(inspector_metric("Ready here", &ready_count.to_string()));
            lines.push(inspector_metric("Services", &service_count.to_string()));

            lines.push(Line::from(""));
            lines.push(inspector_section("PASSENGERS"));
            lines.push(inspector_metric(
                "Waiting",
                &format_population(u64::from(waiting_total)),
            ));
            lines.push(inspector_metric(
                "Arrival rate",
                &format!("+{arrival_rate_total}/h"),
            ));
            lines.push(inspector_metric(
                "Rail adoption",
                &market_maturity_summary(average_maturity_basis_points),
            ));

            if !demand.is_empty() && area.height >= 24 {
                lines.push(Line::from(""));
                lines.push(inspector_section("TOP MARKETS"));
                for (name, waiting, per_hour, maturity) in demand.into_iter().take(3) {
                    lines.push(inspector_destination_line(&name, waiting, per_hour, maturity));
                }
            }
        }

        lines
    } else {
        let mut lines = vec![inspector_metric(
            "Population",
            &format_population(settlement.population),
        )];
        let council_signal = nearest_connection_station_id(&state.region, settlement.id).map(
            |connection_station_id| {
                local_rail_success_basis_points(
                    &state.origin_destination_demand,
                    connection_station_id,
                )
            },
        );

        if compact {
            lines.push(inspector_metric("Rail access", "No station"));
            if let Some(maturity) = council_signal {
                lines.push(inspector_metric(
                    "Council case",
                    &format!(
                        "{} / {}",
                        market_maturity_percent(maturity),
                        market_maturity_percent(
                            COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS
                        )
                    ),
                ));
            } else {
                lines.push(inspector_metric("Passenger rail", "Unavailable"));
            }
        } else {
            lines.push(Line::from(""));
            lines.push(inspector_section("RAIL ACCESS"));
            lines.push(Line::styled("No rail station", theme::primary_value()));
            lines.push(Line::styled(
                "Passenger services require a connection to the rail network.",
                theme::secondary(),
            ));

            if let Some(maturity) = council_signal {
                lines.push(Line::from(""));
                lines.push(inspector_section("COUNCIL CONNECTION CASE"));
                lines.push(inspector_metric(
                    "Nearby adoption",
                    &market_maturity_summary(maturity),
                ));
                lines.push(inspector_metric(
                    "Request threshold",
                    &market_maturity_percent(
                        COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS,
                    ),
                ));
                lines.push(Line::styled(
                    if maturity >= COUNCIL_REQUEST_MATURITY_THRESHOLD_BASIS_POINTS {
                        "Local rail success is strong enough for a council connection request."
                    } else {
                        "Nearby rail use must grow before the council can request a connection."
                    },
                    theme::secondary(),
                ));
            }
        }

        lines
    };

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(&settlement.name, false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn inspector_section(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::secondary().bold())
}

fn inspector_metric(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<15}"), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn inspector_destination_line(
    name: &str,
    waiting: u32,
    per_hour: u32,
    maturity_basis_points: u16,
) -> Line<'static> {
    let name = truncate_label(name, 13);
    Line::from(vec![
        Span::styled(format!("→ {name:<13}"), theme::secondary()),
        Span::styled(
            format!(
                "{waiting} · +{per_hour}/h · {}",
                market_maturity_percent(maturity_basis_points)
            ),
            theme::primary_value(),
        ),
    ])
}

fn market_maturity_summary(basis_points: u16) -> String {
    format!(
        "{} · {}",
        market_maturity_percent(basis_points),
        market_maturity_label(basis_points)
    )
}

fn market_maturity_percent(basis_points: u16) -> String {
    format!("{}%", u32::from(basis_points).saturating_add(50) / 100)
}

fn market_maturity_label(basis_points: u16) -> &'static str {
    match basis_points {
        0..=3_499 => "Emerging",
        3_500..=5_999 => "Growing",
        6_000..=8_499 => "Established",
        _ => "Mature",
    }
}

fn truncate_label(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let prefix = characters.by_ref().take(max_chars).collect::<String>();
    if characters.next().is_some() && max_chars > 1 {
        let mut shortened = prefix.chars().take(max_chars - 1).collect::<String>();
        shortened.push('…');
        shortened
    } else {
        prefix
    }
}

fn operational_layout(state: &GameState) -> Option<OperationalLayout> {
    let network = &state.region.rail_authority.rail_network;
    if state.region.settlements.is_empty() {
        return None;
    }

    let station_by_id = network
        .rail_stations
        .iter()
        .map(|station| (station.id, station))
        .collect::<BTreeMap<_, _>>();
    let station_by_settlement = network
        .rail_stations
        .iter()
        .map(|station| (station.settlement_id, station.id))
        .collect::<BTreeMap<_, _>>();

    let places = state
        .region
        .settlements
        .iter()
        .map(|settlement| OperationalPlace {
            settlement_id: settlement.id,
            station_id: station_by_settlement.get(&settlement.id).copied(),
            name: settlement.name.clone(),
            x: settlement.position.x,
            // Terminal cells are roughly twice as tall as they are wide.
            // Compress world-space Y only for presentation so geography stays
            // visually proportional while the persisted coordinates remain
            // simulation-grade kilometres.
            y: settlement
                .position
                .y
                .div_euclid(TERMINAL_CELL_HEIGHT_TO_WIDTH),
        })
        .collect::<Vec<_>>();

    let lines = network
        .rail_lines
        .iter()
        .filter_map(|line| {
            let first = station_by_id.get(&line.first_station_id)?;
            let second = station_by_id.get(&line.second_station_id)?;
            Some(OperationalLine {
                first_settlement_id: first.settlement_id,
                second_settlement_id: second.settlement_id,
                distance_metres: line.distance.metres(),
                track_count: line.track_count.tracks(),
            })
        })
        .collect();

    Some(OperationalLayout { places, lines })
}

fn render_map_rows(
    layout: &OperationalLayout,
    selected: Option<SettlementId>,
    width: u16,
    height: u16,
    state: &GameState,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let height = usize::from(height);
    let mut grid = vec![vec![MapCell::default(); width]; height];
    let mut rail_mask = vec![vec![0_u8; width]; height];
    let mut rail_accent = vec![vec![false; width]; height];
    let mut rail_double = vec![vec![false; width]; height];

    let min_x = layout.places.iter().map(|place| place.x).min().unwrap_or(0) - 2;
    let max_x = layout.places.iter().map(|place| place.x).max().unwrap_or(0) + 2;
    let min_y = layout.places.iter().map(|place| place.y).min().unwrap_or(0) - 2;
    let max_y = layout.places.iter().map(|place| place.y).max().unwrap_or(0) + 2;
    let logical_width = (max_x - min_x).max(1) as f64;
    let logical_height = (max_y - min_y).max(1) as f64;
    let width_scale = if width <= 2 {
        1.0
    } else {
        (width - 1) as f64 / logical_width
    };
    let height_scale = if height <= 2 {
        1.0
    } else {
        (height - 1) as f64 / logical_height
    };
    // Use one scale for both axes. The logical layout has already compensated
    // for terminal-cell aspect ratio, so stretching X and Y independently
    // would make the same route distance look different by orientation.
    let map_scale = width_scale.min(height_scale).min(1.30);
    let x_scale = map_scale;
    let y_scale = map_scale;
    let scaled_width = (logical_width * x_scale).round() as i32;
    let scaled_height = (logical_height * y_scale).round() as i32;
    let x_padding = ((i32::try_from(width).unwrap_or(i32::MAX) - scaled_width) / 2).max(0);
    let y_padding = ((i32::try_from(height).unwrap_or(i32::MAX) - scaled_height) / 2).max(0);

    let screen_position = |place: &OperationalPlace| {
        let x = (((place.x - min_x) as f64) * x_scale).round() as i32 + x_padding;
        let y = (((place.y - min_y) as f64) * y_scale).round() as i32 + y_padding;
        (x, y)
    };
    let by_id = layout
        .places
        .iter()
        .map(|place| (place.settlement_id, place))
        .collect::<BTreeMap<_, _>>();
    let adjacent = selected_neighbours(layout, selected);

    for line in &layout.lines {
        let (Some(first), Some(second)) = (
            by_id.get(&line.first_settlement_id),
            by_id.get(&line.second_settlement_id),
        ) else {
            continue;
        };
        let start = screen_position(first);
        let end = screen_position(second);
        let accent = selected.is_some_and(|selected_id| {
            selected_id == line.first_settlement_id || selected_id == line.second_settlement_id
        });
        draw_orthogonal_rail(
            &mut rail_mask,
            &mut rail_accent,
            &mut rail_double,
            start,
            end,
            accent,
            line.track_count >= 2,
        );
    }

    for y in 0..height {
        for x in 0..width {
            if rail_mask[y][x] != 0 {
                grid[y][x] = MapCell {
                    ch: rail_glyph(rail_mask[y][x], rail_accent[y][x], rail_double[y][x]),
                    ink: if rail_accent[y][x] {
                        MapInk::RailAccent
                    } else {
                        MapInk::Rail
                    },
                };
            }
        }
    }

    // Draw place markers before moving Trains. A Train that is exactly on a
    // station cell should win visually for that instant, while labels are
    // placed afterwards and therefore avoid both markers and Trains.
    for place in &layout.places {
        let (x, y) = screen_position(place);
        let ink = place_ink(place, selected, &adjacent);
        let marker = if selected == Some(place.settlement_id) {
            '◆'
        } else if place.station_id.is_some() {
            '●'
        } else {
            '○'
        };
        put_cell(&mut grid, x, y, marker, ink);
    }

    let place_positions = layout
        .places
        .iter()
        .map(|place| (place.settlement_id, screen_position(place)))
        .collect::<BTreeMap<_, _>>();
    let station_positions = layout
        .places
        .iter()
        .filter_map(|place| {
            place
                .station_id
                .map(|station_id| (station_id, screen_position(place)))
        })
        .collect::<BTreeMap<_, _>>();
    draw_active_train_markers(
        &mut grid,
        state,
        &station_positions,
        state.last_processed_at,
    );

    // Labels follow the same focus hierarchy as the markers. Place the current
    // focus first, then its direct neighbours, so background labels cannot steal
    // the best collision-free position from the context the player is operating.
    // Operational counts live in the inspector instead of being repeated beside
    // every station name.
    let mut label_places = layout.places.iter().collect::<Vec<_>>();
    label_places.sort_by_key(|place| focus_rank(place, selected, &adjacent));
    for place in label_places {
        let (x, y) = screen_position(place);
        let label = map_place_label(place, selected);
        let preferred_direction =
            preferred_label_direction(place, &label, &layout.places, &place_positions, selected);
        place_map_label(
            &mut grid,
            x,
            y,
            &label,
            place_ink(place, selected, &adjacent),
            preferred_direction,
        );
    }

    // Exact distances appear only for Rail Links incident to the current
    // selection. If every candidate would collide with a place label, Train,
    // or rail geometry, omit the map annotation; the inspector still carries
    // the exact distance.
    if let Some(selected_id) = selected {
        for line in layout.lines.iter().filter(|line| {
            line.first_settlement_id == selected_id || line.second_settlement_id == selected_id
        }) {
            let (Some(first), Some(second)) = (
                by_id.get(&line.first_settlement_id),
                by_id.get(&line.second_settlement_id),
            ) else {
                continue;
            };
            place_link_distance_label(
                &mut grid,
                screen_position(first),
                screen_position(second),
                line.distance_metres,
            );
        }
    }

    grid.into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            let mut current_ink = MapInk::Empty;
            let mut current = String::new();
            for cell in row {
                if !current.is_empty() && cell.ink != current_ink {
                    spans.push(Span::styled(current, map_ink_style(current_ink)));
                    current = String::new();
                }
                current_ink = cell.ink;
                current.push(cell.ch);
            }
            if !current.is_empty() {
                spans.push(Span::styled(current, map_ink_style(current_ink)));
            }
            Line::from(spans)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JourneyRouteSegment {
    from_station_id: RailStationId,
    to_station_id: RailStationId,
    distance_metres: u64,
}

fn draw_active_train_markers(
    grid: &mut [Vec<MapCell>],
    state: &GameState,
    station_positions: &BTreeMap<RailStationId, (i32, i32)>,
    now: UtcSeconds,
) {
    let mut markers = BTreeMap::<(i32, i32), (char, usize)>::new();
    for journey in &state.active_journeys {
        let Some((position, glyph)) = journey_map_marker(state, journey, station_positions, now)
        else {
            continue;
        };
        markers
            .entry(position)
            .and_modify(|(_, count)| *count = count.saturating_add(1))
            .or_insert((glyph, 1));
    }

    for (position, (direction, count)) in markers {
        let glyph = match count {
            1 => direction,
            2..=9 => char::from_digit(u32::try_from(count).unwrap_or(9), 10).unwrap_or('+'),
            _ => '+',
        };
        put_cell(grid, position.0, position.1, glyph, MapInk::Train);
    }
}

fn journey_map_marker(
    state: &GameState,
    journey: &Journey,
    station_positions: &BTreeMap<RailStationId, (i32, i32)>,
    now: UtcSeconds,
) -> Option<((i32, i32), char)> {
    let segments = journey_route_segments(state, journey)?;
    let total_distance = segments.iter().try_fold(0_u64, |total, segment| {
        total.checked_add(segment.distance_metres)
    })?;
    if total_distance == 0 {
        return None;
    }

    let total_seconds = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .max(1);
    let elapsed_seconds = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, total_seconds);
    let travelled_metres = total_distance as f64 * (elapsed_seconds as f64 / total_seconds as f64);

    let mut distance_before = 0.0_f64;
    for (index, segment) in segments.iter().enumerate() {
        let segment_distance = segment.distance_metres as f64;
        let distance_after = distance_before + segment_distance;
        if travelled_metres <= distance_after || index + 1 == segments.len() {
            let local_progress = if segment_distance <= f64::EPSILON {
                0.0
            } else {
                ((travelled_metres - distance_before) / segment_distance).clamp(0.0, 1.0)
            };
            let start = station_positions.get(&segment.from_station_id).copied()?;
            let end = station_positions.get(&segment.to_station_id).copied()?;
            return Some(point_along_orthogonal_rail(start, end, local_progress));
        }
        distance_before = distance_after;
    }

    None
}

fn journey_route_segments(
    state: &GameState,
    journey: &Journey,
) -> Option<Vec<JourneyRouteSegment>> {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)?;
    let network = &state.region.rail_authority.rail_network;
    let direction = journey_service_direction(service, journey)?;
    let next_index = if direction > 0 {
        journey.current_stop_index.checked_add(1)?
    } else {
        journey.current_stop_index.checked_sub(1)?
    };
    let from_station_id = *service.stop_station_ids.get(journey.current_stop_index)?;
    let to_station_id = *service.stop_station_ids.get(next_index)?;
    let line_ids = path_between_stations(network, from_station_id, to_station_id).ok()?;

    let mut current_station_id = from_station_id;
    let mut segments = Vec::with_capacity(line_ids.len());
    for rail_line_id in line_ids {
        let line = network
            .rail_lines
            .iter()
            .find(|line| line.id == rail_line_id)?;
        let next_station_id = if line.first_station_id == current_station_id {
            line.second_station_id
        } else if line.second_station_id == current_station_id {
            line.first_station_id
        } else {
            return None;
        };
        segments.push(JourneyRouteSegment {
            from_station_id: current_station_id,
            to_station_id: next_station_id,
            distance_metres: line.distance.metres(),
        });
        current_station_id = next_station_id;
    }

    (current_station_id == to_station_id).then_some(segments)
}

fn journey_service_direction(
    service: &crate::model::PassengerService,
    journey: &Journey,
) -> Option<i32> {
    let first = service.stop_station_ids.first().copied()?;
    let last = service.stop_station_ids.last().copied()?;
    if journey.origin_station_id == first && journey.destination_station_id == last {
        Some(1)
    } else if journey.origin_station_id == last && journey.destination_station_id == first {
        Some(-1)
    } else {
        None
    }
}

fn journey_next_stop_station_id(state: &GameState, journey: &Journey) -> Option<RailStationId> {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)?;
    let direction = journey_service_direction(service, journey)?;
    let index = if direction > 0 {
        journey.current_stop_index.checked_add(1)?
    } else {
        journey.current_stop_index.checked_sub(1)?
    };
    service.stop_station_ids.get(index).copied()
}

fn point_along_orthogonal_rail(
    start: (i32, i32),
    end: (i32, i32),
    progress: f64,
) -> ((i32, i32), char) {
    let horizontal_steps = (end.0 - start.0).unsigned_abs();
    let vertical_steps = (end.1 - start.1).unsigned_abs();
    let total_steps = horizontal_steps.saturating_add(vertical_steps);
    if total_steps == 0 {
        return (start, '▶');
    }

    let progress = progress.clamp(0.0, 1.0);
    let mut travelled_steps = if progress <= 0.0 {
        0
    } else if progress >= 1.0 {
        total_steps
    } else {
        ((progress * f64::from(total_steps)).floor() as u32)
            .max(1)
            .min(total_steps.saturating_sub(1).max(1))
    };

    if travelled_steps <= horizontal_steps && horizontal_steps > 0 {
        let direction = (end.0 - start.0).signum();
        let x = start.0.saturating_add(
            direction.saturating_mul(i32::try_from(travelled_steps).unwrap_or(i32::MAX)),
        );
        return ((x, start.1), if direction >= 0 { '▶' } else { '◀' });
    }

    travelled_steps = travelled_steps.saturating_sub(horizontal_steps);
    let direction = (end.1 - start.1).signum();
    let y = start.1.saturating_add(
        direction.saturating_mul(i32::try_from(travelled_steps).unwrap_or(i32::MAX)),
    );
    ((end.0, y), if direction >= 0 { '▼' } else { '▲' })
}

fn selected_neighbours(
    layout: &OperationalLayout,
    selected: Option<SettlementId>,
) -> BTreeSet<SettlementId> {
    let Some(selected) = selected else {
        return BTreeSet::new();
    };

    layout
        .lines
        .iter()
        .filter_map(|line| {
            if line.first_settlement_id == selected {
                Some(line.second_settlement_id)
            } else if line.second_settlement_id == selected {
                Some(line.first_settlement_id)
            } else {
                None
            }
        })
        .collect()
}

fn place_ink(
    place: &OperationalPlace,
    selected: Option<SettlementId>,
    adjacent: &BTreeSet<SettlementId>,
) -> MapInk {
    if selected == Some(place.settlement_id) {
        MapInk::Selected
    } else if adjacent.contains(&place.settlement_id) {
        MapInk::ConnectedAdjacent
    } else if place.station_id.is_some() {
        MapInk::Connected
    } else {
        MapInk::Unconnected
    }
}

fn focus_rank(
    place: &OperationalPlace,
    selected: Option<SettlementId>,
    adjacent: &BTreeSet<SettlementId>,
) -> u8 {
    if selected == Some(place.settlement_id) {
        0
    } else if adjacent.contains(&place.settlement_id) {
        1
    } else if place.station_id.is_some() {
        2
    } else {
        3
    }
}

fn map_place_label(place: &OperationalPlace, selected: Option<SettlementId>) -> String {
    if selected == Some(place.settlement_id) {
        place.name.to_uppercase()
    } else {
        place.name.clone()
    }
}

fn place_link_distance_label(
    grid: &mut [Vec<MapCell>],
    start: (i32, i32),
    end: (i32, i32),
    distance_metres: u64,
) {
    let text = format_distance(distance_metres);
    let text_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let horizontal_steps = (end.0 - start.0).abs();
    let vertical_steps = (end.1 - start.1).abs();

    let candidates = if horizontal_steps >= vertical_steps && horizontal_steps > 0 {
        let midpoint_x = start.0 + (end.0 - start.0) / 2;
        let start_x = midpoint_x - text_width / 2;
        vec![(start_x, start.1 - 1), (start_x, start.1 + 1)]
    } else {
        let midpoint_y = start.1 + (end.1 - start.1) / 2;
        let rail_x = end.0;
        vec![
            (rail_x + 2, midpoint_y),
            (rail_x - text_width - 2, midpoint_y),
        ]
    };

    if let Some((x, y)) = candidates
        .into_iter()
        .find(|(x, y)| can_place_text(grid, *x, *y, &text))
    {
        put_text(grid, x, y, &text, MapInk::RailLabel);
    }
}

fn preferred_label_direction(
    place: &OperationalPlace,
    label: &str,
    places: &[OperationalPlace],
    positions: &BTreeMap<SettlementId, (i32, i32)>,
    selected: Option<SettlementId>,
) -> Option<MapDirection> {
    let &(x, y) = positions.get(&place.settlement_id)?;
    let own_width = i32::try_from(label.chars().count()).unwrap_or(i32::MAX);

    places
        .iter()
        .filter(|other| other.settlement_id != place.settlement_id)
        .filter_map(|other| {
            let &(other_x, other_y) = positions.get(&other.settlement_id)?;
            let other_label = map_place_label(other, selected);
            let other_width = i32::try_from(other_label.chars().count()).unwrap_or(i32::MAX);
            let dx = other_x - x;
            let dy = other_y - y;
            let horizontal = dx.abs() >= dy.abs();
            let crowded = if horizontal {
                dy.abs() <= 2 && dx.abs() <= ((own_width + other_width) / 2 + 6).max(12)
            } else {
                dx.abs() <= 3 && dy.abs() <= 6
            };
            crowded.then_some((dx.abs() + dy.abs(), dx, dy))
        })
        .min_by_key(|(distance, _, _)| *distance)
        .map(|(_, dx, dy)| {
            if dx.abs() >= dy.abs() {
                if dx > 0 {
                    MapDirection::Left
                } else {
                    MapDirection::Right
                }
            } else if dy > 0 {
                MapDirection::Up
            } else {
                MapDirection::Down
            }
        })
}

fn place_map_label(
    grid: &mut [Vec<MapCell>],
    marker_x: i32,
    marker_y: i32,
    text: &str,
    ink: MapInk,
    preferred_direction: Option<MapDirection>,
) {
    let label_width = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let centered_x = marker_x - label_width / 2;
    let position_for = |direction| match direction {
        MapDirection::Up => (centered_x, marker_y - 1),
        MapDirection::Down => (centered_x, marker_y + 1),
        MapDirection::Right => (marker_x + 2, marker_y),
        MapDirection::Left => (marker_x - label_width - 2, marker_y),
    };

    let mut candidates = Vec::with_capacity(4);
    if let Some(direction) = preferred_direction {
        candidates.push(position_for(direction));
    }
    for direction in [
        MapDirection::Up,
        MapDirection::Down,
        MapDirection::Right,
        MapDirection::Left,
    ] {
        let candidate = position_for(direction);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    if let Some((x, y)) = candidates
        .into_iter()
        .find(|(x, y)| can_place_text(grid, *x, *y, text))
    {
        put_text(grid, x, y, text, ink);
        return;
    }

    // Very small terminals may leave no collision-free row. In that case,
    // prefer the normal centred-above placement and let put_text clip safely.
    put_text(grid, centered_x, marker_y - 1, text, ink);
}

fn can_place_text(grid: &[Vec<MapCell>], x: i32, y: i32, text: &str) -> bool {
    let Ok(y) = usize::try_from(y) else {
        return false;
    };
    let Some(row) = grid.get(y) else {
        return false;
    };
    let Ok(start_x) = usize::try_from(x) else {
        return false;
    };
    let text_width = text.chars().count();
    let Some(end_x) = start_x.checked_add(text_width) else {
        return false;
    };
    if end_x > row.len() {
        return false;
    }

    row[start_x..end_x]
        .iter()
        .all(|cell| cell.ink == MapInk::Empty)
}

fn draw_orthogonal_rail(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    start: (i32, i32),
    end: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let corner = (end.0, start.1);
    draw_segment(masks, accents, doubles, start, corner, accent, double_track);
    draw_segment(masks, accents, doubles, corner, end, accent, double_track);
}

fn draw_segment(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    start: (i32, i32),
    end: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let (mut x, mut y) = start;
    while (x, y) != end {
        let next = if x < end.0 {
            (x + 1, y)
        } else if x > end.0 {
            (x - 1, y)
        } else if y < end.1 {
            (x, y + 1)
        } else {
            (x, y - 1)
        };
        add_rail_connection(masks, accents, doubles, (x, y), next, accent, double_track);
        x = next.0;
        y = next.1;
    }
}

fn add_rail_connection(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    from: (i32, i32),
    to: (i32, i32),
    accent: bool,
    double_track: bool,
) {
    let (from_bit, to_bit) = match (to.0 - from.0, to.1 - from.1) {
        (1, 0) => (RAIL_RIGHT, RAIL_LEFT),
        (-1, 0) => (RAIL_LEFT, RAIL_RIGHT),
        (0, 1) => (RAIL_DOWN, RAIL_UP),
        (0, -1) => (RAIL_UP, RAIL_DOWN),
        _ => return,
    };
    add_rail_bit(
        masks,
        accents,
        doubles,
        from,
        from_bit,
        accent,
        double_track,
    );
    add_rail_bit(masks, accents, doubles, to, to_bit, accent, double_track);
}

fn add_rail_bit(
    masks: &mut [Vec<u8>],
    accents: &mut [Vec<bool>],
    doubles: &mut [Vec<bool>],
    (x, y): (i32, i32),
    bit: u8,
    accent: bool,
    double_track: bool,
) {
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return;
    };
    let Some(row) = masks.get_mut(y) else {
        return;
    };
    let Some(mask) = row.get_mut(x) else {
        return;
    };
    *mask |= bit;
    if accent {
        if let Some(row) = accents.get_mut(y) {
            if let Some(value) = row.get_mut(x) {
                *value = true;
            }
        }
    }
    if double_track {
        if let Some(row) = doubles.get_mut(y) {
            if let Some(value) = row.get_mut(x) {
                *value = true;
            }
        }
    }
}

fn rail_glyph(mask: u8, accent: bool, double_track: bool) -> char {
    if double_track {
        double_rail_glyph(mask)
    } else if accent {
        heavy_rail_glyph(mask)
    } else {
        light_rail_glyph(mask)
    }
}

fn double_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '═',
        m if m == (RAIL_UP | RAIL_DOWN) => '║',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '╔',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '╗',
        m if m == (RAIL_RIGHT | RAIL_UP) => '╚',
        m if m == (RAIL_LEFT | RAIL_UP) => '╝',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '╦',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '╩',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '╠',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '╣',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '╬',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '╬',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '═',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '║',
        _ => '·',
    }
}

fn heavy_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '━',
        m if m == (RAIL_UP | RAIL_DOWN) => '┃',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '┏',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '┓',
        m if m == (RAIL_RIGHT | RAIL_UP) => '┗',
        m if m == (RAIL_LEFT | RAIL_UP) => '┛',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '┳',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '┻',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '┣',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '┫',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '╋',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '╋',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '━',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '┃',
        _ => '·',
    }
}

fn light_rail_glyph(mask: u8) -> char {
    match mask {
        m if m == (RAIL_LEFT | RAIL_RIGHT) => '─',
        m if m == (RAIL_UP | RAIL_DOWN) => '│',
        m if m == (RAIL_RIGHT | RAIL_DOWN) => '┌',
        m if m == (RAIL_LEFT | RAIL_DOWN) => '┐',
        m if m == (RAIL_RIGHT | RAIL_UP) => '└',
        m if m == (RAIL_LEFT | RAIL_UP) => '┘',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_DOWN) => '┬',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP) => '┴',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_RIGHT) => '├',
        m if m == (RAIL_UP | RAIL_DOWN | RAIL_LEFT) => '┤',
        m if m == (RAIL_LEFT | RAIL_RIGHT | RAIL_UP | RAIL_DOWN) => '┼',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 && m & (RAIL_UP | RAIL_DOWN) != 0 => '┼',
        m if m & (RAIL_LEFT | RAIL_RIGHT) != 0 => '─',
        m if m & (RAIL_UP | RAIL_DOWN) != 0 => '│',
        _ => '·',
    }
}

fn put_cell(grid: &mut [Vec<MapCell>], x: i32, y: i32, ch: char, ink: MapInk) {
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return;
    };
    if let Some(cell) = grid.get_mut(y).and_then(|row| row.get_mut(x)) {
        *cell = MapCell { ch, ink };
    }
}

fn put_text(grid: &mut [Vec<MapCell>], x: i32, y: i32, text: &str, ink: MapInk) {
    for (offset, ch) in text.chars().enumerate() {
        let offset = i32::try_from(offset).unwrap_or(i32::MAX);
        put_cell(grid, x.saturating_add(offset), y, ch, ink);
    }
}

fn map_ink_style(ink: MapInk) -> Style {
    match ink {
        MapInk::Empty => theme::panel(),
        MapInk::Rail => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::RailAccent => theme::focused_title(),
        MapInk::RailLabel => theme::focused_border(),
        MapInk::Connected => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::ConnectedAdjacent => theme::primary_value().add_modifier(Modifier::BOLD),
        MapInk::Unconnected => theme::secondary().add_modifier(Modifier::DIM),
        MapInk::Selected => theme::focused_title(),
        MapInk::Train => theme::warning().add_modifier(Modifier::BOLD),
    }
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

/// Presentation-only selection for the active departure board.
///
/// A selected Journey is identified by its stable ID. Its Train ID is retained
/// separately, so reconciliation can remove the arrived Journey while the
/// Fleet still opens on that now-READY Train.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JourneySelection {
    selected_journey_id: Option<JourneyId>,
    selected_train_id: Option<TrainId>,
    table_state: TableState,
    page_size: usize,
}

/// The three independent Map browse selections supplied to one render pass.
/// Grouping them keeps the dashboard interface focused as more Map panels are
/// added without coupling presentation state to the simulation.
pub struct MapSelections<'a> {
    pub stations: &'a mut StationSelection,
    pub settlements: &'a mut SettlementSelection,
    pub journeys: &'a mut JourneySelection,
}

impl JourneySelection {
    /// Moves through active Journeys without changing the simulation.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let last = state.active_journeys.len().saturating_sub(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected.saturating_add(1).min(last),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected.saturating_add(page_size).min(last),
            _ => selected,
        };
        self.select_index(state, next);
    }

    /// Returns the selected active Journey after reconciling its stable ID.
    pub fn selected_journey_id(&mut self, state: &GameState) -> Option<JourneyId> {
        self.synchronize(state);
        self.selected_journey_id
    }

    /// Returns the Train associated with the last explicitly selected Journey.
    /// It intentionally survives an arrival so Fleet selection remains useful.
    pub fn selected_train_id(&mut self, state: &GameState) -> Option<TrainId> {
        self.synchronize(state);
        self.selected_train_id
    }

    fn synchronize(&mut self, state: &GameState) {
        let journeys = &state.active_journeys;
        let previous_index = self.table_state.selected().unwrap_or(0);
        let retained = self
            .selected_journey_id
            .and_then(|journey_id| journeys.iter().position(|journey| journey.id == journey_id));
        let fallback = if journeys.is_empty() {
            None
        } else {
            Some(previous_index.min(journeys.len() - 1))
        };
        let selected = retained.or(fallback);
        if let Some(index) = selected {
            self.selected_journey_id = Some(journeys[index].id);
            if self.selected_train_id.is_none() {
                self.selected_train_id = Some(journeys[index].train_id);
            }
        } else {
            self.selected_journey_id = None;
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(journey) = state.active_journeys.get(index) else {
            return;
        };
        self.selected_journey_id = Some(journey.id);
        self.selected_train_id = Some(journey.train_id);
        self.table_state.select(Some(index));
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }

    fn keep_compact_selection_visible(&mut self, visible_items: usize) {
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let visible_items = visible_items.max(1);
        let offset = self.table_state.offset();
        if selected < offset {
            *self.table_state.offset_mut() = selected;
        } else if selected >= offset.saturating_add(visible_items) {
            *self.table_state.offset_mut() =
                selected.saturating_add(1).saturating_sub(visible_items);
        }
    }
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

fn render_journey_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
) {
    let [board_area, inspector_area] =
        Layout::horizontal([Constraint::Min(58), Constraint::Length(32)])
            .spacing(1)
            .areas(area);
    render_journey_table(frame, board_area, state, now, selection, true);
    render_journey_inspector(frame, inspector_area, state, now, selection, false);
}

fn render_journey_table(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
    focused: bool,
) {
    let visible_items = usize::from(area.height.saturating_sub(4)).max(1);
    selection.set_page_size(visible_items);
    let rows = state
        .active_journeys
        .iter()
        .map(|journey| {
            Row::new([
                Cell::from(format!("Train {:02}", journey.train_id.get())),
                Cell::from(station_name(state, journey.origin_station_id)),
                Cell::from(
                    journey_next_stop_station_id(state, journey)
                        .map(|station_id| station_name(state, station_id))
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
                Cell::from(format!(
                    "in {}",
                    format_duration(remaining_seconds(journey, now))
                )),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(31),
            Constraint::Percentage(31),
            Constraint::Length(11),
        ],
    )
    .header(
        Row::new(["Train", "Origin", "Next stop", "ETA"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .block(panel_block("Departure Board · active Journeys", focused))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol(theme::SELECTION_MARKER)
    .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

fn render_compact_journey_board(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
) {
    if state.active_journeys.is_empty() {
        render_empty_journey_board(frame, area, true);
        return;
    }
    let visible_items = usize::from(area.height.saturating_sub(2) / 3).max(1);
    selection.set_page_size(visible_items);
    selection.keep_compact_selection_visible(visible_items);
    let offset = selection.table_state.offset();
    let selected = selection.table_state.selected();
    let lines = state
        .active_journeys
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_items)
        .flat_map(|(index, journey)| {
            compact_journey_lines(state, journey, now, selected == Some(index))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Departure Board · active Journeys", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_journey_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
    focused: bool,
) {
    if state.active_journeys.is_empty() {
        render_empty_journey_board(frame, area, focused);
        return;
    }
    let block = panel_block("Selected Journey", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let selected = selection.selected_journey_id(state).and_then(|journey_id| {
        state
            .active_journeys
            .iter()
            .find(|journey| journey.id == journey_id)
    });
    let Some(journey) = selected else {
        frame.render_widget(
            Paragraph::new("No active Journey selected.").style(theme::panel()),
            inner,
        );
        return;
    };
    let percent = u16::try_from(journey_progress_percent(journey, now)).unwrap_or(100);
    let [details_area, gauge_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let lines = vec![
        Line::styled(
            format!("Train {:02}", journey.train_id.get()),
            theme::title(),
        ),
        labelled_line(
            "Route",
            &format!(
                "{} → {}",
                station_name(state, journey.origin_station_id),
                station_name(state, journey.destination_station_id)
            ),
        ),
        labelled_line(
            "Next stop",
            &journey_next_stop_station_id(state, journey)
                .map(|station_id| station_name(state, station_id).to_owned())
                .unwrap_or_else(|| "unknown".into()),
        ),
        labelled_line(
            "ETA",
            &format!("in {}", format_duration(remaining_seconds(journey, now))),
        ),
        labelled_line("On board", &journey.onboard_passengers().to_string()),
        labelled_line("Leg progress", &format!("{percent}%")),
        Line::from(""),
        Line::styled(
            "Map marker follows the current Service leg.",
            theme::secondary(),
        ),
        Line::styled("Real-time Journeys continue", theme::hint()),
        Line::styled("after exit.", theme::hint()),
        Line::from(""),
        Line::styled(
            if focused {
                "Esc · departure board    Tab · Rail Stations"
            } else {
                "Enter · focused details    Tab · next Map panel"
            },
            theme::hint(),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        details_area,
    );
    frame.render_widget(
        LineGauge::default()
            .ratio(f64::from(percent) / 100.0)
            .label(format!("{percent}%"))
            .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
            .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
        gauge_area,
    );
}

fn render_empty_journey_board(frame: &mut Frame, area: Rect, focused: bool) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("No active Journeys", theme::title()),
            Line::styled(
                "Press D anywhere on the Map to choose from all READY Trains.",
                theme::secondary(),
            ),
            Line::styled("Journeys continue in real time after exit.", theme::hint()),
        ])
        .block(panel_block("Departure Board · active Journeys", focused))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn compact_journey_lines(
    state: &GameState,
    journey: &Journey,
    now: UtcSeconds,
    selected: bool,
) -> [Line<'static>; 3] {
    let marker = if selected { ">" } else { " " };
    let row_style = selected
        .then(theme::selected_row)
        .unwrap_or_else(theme::panel);
    [
        Line::styled(
            format!(
                "{marker} Train {:02}  ETA in {}",
                journey.train_id.get(),
                format_duration(remaining_seconds(journey, now))
            ),
            row_style.add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(
                "  {} → {}",
                station_name(state, journey.origin_station_id),
                station_name(state, journey.destination_station_id)
            ),
            row_style,
        ),
        Line::styled(
            format!("  {}% to next stop", journey_progress_percent(journey, now)),
            row_style,
        ),
    ]
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

fn format_population(population: u64) -> String {
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

fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
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
    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{
        MapCell, MapDirection, RAIL_LEFT, RAIL_RIGHT, TERMINAL_CELL_HEIGHT_TO_WIDTH,
        directional_line_length, focus_rank, journey_progress_percent, journey_route_segments,
        map_place_label, operational_layout, place_link_distance_label,
        point_along_orthogonal_rail, rail_glyph, render_at, schematic_layout, selected_neighbours,
    };

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
    fn map_distance_is_comparable_across_horizontal_and_vertical_links() {
        let same_distance_horizontal = directional_line_length(42_000, MapDirection::Right);
        let same_distance_vertical = directional_line_length(42_000, MapDirection::Down);
        assert!(
            (same_distance_horizontal - same_distance_vertical * TERMINAL_CELL_HEIGHT_TO_WIDTH)
                .abs()
                <= 1
        );

        let shorter_vertical =
            directional_line_length(31_000, MapDirection::Down) * TERMINAL_CELL_HEIGHT_TO_WIDTH;
        let longer_horizontal = directional_line_length(42_000, MapDirection::Right);
        assert!(longer_horizontal > shorter_vertical);
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
