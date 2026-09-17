use std::collections::BTreeMap;

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::{
    model::{GameState, RailStationId, ServiceDirectionMode, ServiceId},
    sim::services::service_path_for_stops,
};

use super::{ServiceWorkspaceAction, station_label, truncate_display};
use crate::ui::{format, modal, theme};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct CreateServiceFlow {
    stop_station_ids: Vec<RailStationId>,
    selected_station_index: usize,
    review: bool,
    error: Option<String>,
    direction_mode: ServiceDirectionMode,
    editing_service_id: Option<ServiceId>,
}

impl CreateServiceFlow {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn editing(
        stop_station_ids: Vec<RailStationId>,
        selected_station_index: usize,
        direction_mode: ServiceDirectionMode,
        service_id: ServiceId,
    ) -> Self {
        Self {
            stop_station_ids,
            selected_station_index,
            review: false,
            error: None,
            direction_mode,
            editing_service_id: Some(service_id),
        }
    }

    pub(super) fn handle_key(
        &mut self,
        key: KeyCode,
        state: &GameState,
    ) -> ServiceWorkspaceAction {
        if self.review {
            return match key {
                KeyCode::Enter => match self.editing_service_id {
                    Some(service_id) => ServiceWorkspaceAction::Update {
                        service_id,
                        stop_station_ids: self.stop_station_ids.clone(),
                        direction_mode: self.direction_mode,
                    },
                    None => ServiceWorkspaceAction::Create {
                        stop_station_ids: self.stop_station_ids.clone(),
                        direction_mode: self.direction_mode,
                    },
                },
                KeyCode::Backspace | KeyCode::Left => {
                    self.review = false;
                    self.error = None;
                    ServiceWorkspaceAction::Continue
                }
                _ => ServiceWorkspaceAction::Continue,
            };
        }

        let station_count = state.region.rail_authority.rail_network.rail_stations.len();
        match key {
            KeyCode::Up | KeyCode::Char('k' | 'K') => {
                self.selected_station_index = self.selected_station_index.saturating_sub(1);
                self.error = None;
            }
            KeyCode::Down | KeyCode::Char('j' | 'J') => {
                if station_count > 0 {
                    self.selected_station_index =
                        (self.selected_station_index + 1).min(station_count.saturating_sub(1));
                }
                self.error = None;
            }
            KeyCode::Char(' ') => {
                let Some(station_id) = state
                    .region
                    .rail_authority
                    .rail_network
                    .rail_stations
                    .get(self.selected_station_index)
                    .map(|station| station.id)
                else {
                    return ServiceWorkspaceAction::Continue;
                };

                if let Some(selected_index) = self
                    .stop_station_ids
                    .iter()
                    .position(|selected_id| *selected_id == station_id)
                {
                    self.stop_station_ids.remove(selected_index);
                    self.error = None;
                    return ServiceWorkspaceAction::Continue;
                }

                let mut candidate = self.stop_station_ids.clone();
                candidate.push(station_id);
                if candidate.len() == 1 {
                    self.stop_station_ids = candidate;
                    self.error = None;
                } else {
                    match service_path_for_stops(
                        &state.region.rail_authority.rail_network,
                        &candidate,
                    ) {
                        Ok(_) => {
                            self.stop_station_ids = candidate;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            KeyCode::Char('m' | 'M') => {
                self.direction_mode = match self.direction_mode {
                    ServiceDirectionMode::BothDirections => ServiceDirectionMode::ForwardOnly,
                    ServiceDirectionMode::ForwardOnly => ServiceDirectionMode::BothDirections,
                };
                self.error = None;
            }
            KeyCode::Enter => {
                if self.stop_station_ids.len() < 2 {
                    self.error =
                        Some("Select at least two stops before reviewing the Service.".into());
                } else {
                    match service_path_for_stops(
                        &state.region.rail_authority.rail_network,
                        &self.stop_station_ids,
                    ) {
                        Ok(_) => {
                            self.review = true;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            _ => {}
        }
        ServiceWorkspaceAction::Continue
    }

    pub(super) fn footer_shortcuts(
        &self,
        compact: bool,
    ) -> Vec<(&'static str, &'static str, bool)> {
        if self.review {
            vec![
                (
                    "Enter",
                    if self.editing_service_id.is_some() {
                        "Save"
                    } else {
                        "Create"
                    },
                    true,
                ),
                ("←", "Edit", true),
                ("Esc", "Cancel", true),
            ]
        } else if compact {
            vec![
                ("↑↓", "Station", true),
                ("Space", "Toggle", true),
                ("M", "Direction", true),
                ("Enter", "Review", true),
                ("Esc", "Cancel", true),
            ]
        } else {
            vec![
                ("↑↓/JK", "Station", true),
                ("Space", "Toggle stop", true),
                ("M", "Direction", true),
                ("Enter", "Review", true),
                ("Esc", "Cancel", true),
            ]
        }
    }

    pub(super) fn reject(&mut self, message: impl Into<String>) {
        self.review = false;
        self.error = Some(message.into());
    }
}

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &CreateServiceFlow,
) {
    let title = flow
        .editing_service_id
        .and_then(|service_id| {
            state
                .player_company
                .passenger_services
                .iter()
                .find(|service| service.id == service_id)
                .map(|service| format!("Edit Passenger Service · {}", service.display_name()))
        })
        .unwrap_or_else(|| "Create Passenger Service".to_owned());
    let modal_areas = modal::render_shell(
        frame,
        area,
        &title,
        footer_line(flow, area.width),
    );

    if flow.review {
        render_review(frame, modal_areas.body, state, flow);
    } else {
        render_picker(frame, modal_areas.body, state, flow);
    }
}

fn footer_line(flow: &CreateServiceFlow, width: u16) -> Line<'static> {
    if flow.review {
        return modal::shortcut_line(&[
            (
                "Enter",
                if flow.editing_service_id.is_some() {
                    "save"
                } else {
                    "create"
                },
            ),
            ("←", "edit"),
            ("Esc", "cancel"),
        ]);
    }

    if width >= 76 {
        modal::shortcut_line(&[
            ("↑/↓", "choose"),
            ("Space", "toggle"),
            ("M", "direction"),
            ("Enter", "review"),
            ("Esc", "cancel"),
        ])
    } else {
        modal::shortcut_line(&[
            ("Space", "toggle"),
            ("M", "direction"),
            ("Enter", "review"),
            ("Esc", "cancel"),
        ])
    }
}

fn render_picker(frame: &mut Frame, area: Rect, state: &GameState, flow: &CreateServiceFlow) {
    let status_rows = if flow.error.is_some() { 2 } else { 0 };
    let [context_area, content_area, status_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(status_rows),
    ])
    .areas(area);

    let route_summary = if flow.stop_station_ids.is_empty() {
        "No stops selected yet".to_owned()
    } else {
        route_pattern_label(state, &flow.stop_station_ids, flow.direction_mode)
    };
    frame.render_widget(
        Paragraph::new(vec![
            step_line(1),
            Line::from(vec![
                Span::styled("ROUTE  ", theme::secondary()),
                Span::styled(
                    truncate_display(
                        &route_summary,
                        context_area.width.saturating_sub(7) as usize,
                    ),
                    theme::primary_value(),
                ),
            ]),
            Line::from(vec![
                Span::styled("MODE   ", theme::secondary()),
                Span::styled(direction_mode_label(flow.direction_mode), theme::primary_value()),
                Span::styled(" · M to toggle", theme::secondary()),
            ]),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    let show_preview = content_area.width >= 68 && content_area.height >= 7;
    let (picker_area, divider_area, preview_area) = if show_preview {
        let [picker_area, divider_area, preview_area] = Layout::horizontal([
            Constraint::Min(30),
            Constraint::Length(1),
            Constraint::Length(32),
        ])
        .areas(content_area);
        (picker_area, Some(divider_area), Some(preview_area))
    } else {
        (content_area, None, None)
    };

    let [picker_title_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(picker_area);
    frame.render_widget(
        Paragraph::new(Line::styled("Choose stops", theme::title())).style(theme::panel()),
        picker_title_area,
    );

    let rows = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| {
            let selection = flow
                .stop_station_ids
                .iter()
                .position(|station_id| *station_id == station.id)
                .map(|index| format!("[{}]", index + 1))
                .unwrap_or_else(|| "[ ]".to_owned());
            Row::new([selection, station_label(state, station.id)])
        })
        .collect::<Vec<_>>();
    let mut table_state = TableState::default();
    table_state.select(
        (!rows.is_empty()).then_some(
            flow.selected_station_index
                .min(rows.len().saturating_sub(1)),
        ),
    );
    let table = Table::new(rows, [Constraint::Length(4), Constraint::Min(1)])
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut table_state);

    if let (Some(divider_area), Some(preview_area)) = (divider_area, preview_area) {
        modal::render_vertical_separator(frame, divider_area);
        render_preview(frame, preview_area, state, flow);
    }

    if let Some(error) = &flow.error {
        frame.render_widget(
            Paragraph::new(Line::styled(error.clone(), theme::error()))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
}

fn render_preview(frame: &mut Frame, area: Rect, state: &GameState, flow: &CreateServiceFlow) {
    let selected_station_id = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .get(flow.selected_station_index)
        .map(|station| station.id);

    let map_height = area.height.saturating_sub(8).clamp(4, 9);
    let [title_area, map_area, legend_area, details_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(map_height),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::styled("Route Preview", theme::title())).style(theme::panel()),
        title_area,
    );
    render_route_map(frame, map_area, state, flow, selected_station_id);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("━", theme::focused_title()),
            Span::styled(" route  ", theme::secondary()),
            Span::styled("━", theme::success()),
            Span::styled(" preview  ", theme::secondary()),
            Span::styled("◆", theme::warning()),
            Span::styled(" cursor", theme::secondary()),
        ]))
        .style(theme::panel()),
        legend_area,
    );

    let mut lines = Vec::new();
    if flow.stop_station_ids.is_empty() {
        lines.push(Line::styled(
            "The first stop becomes the Service origin.",
            theme::secondary(),
        ));
    } else {
        lines.push(Line::styled("ORDERED STOPS", theme::table_header()));
        let max_stop_rows = details_area.height.saturating_sub(3).max(1) as usize;
        lines.extend(preview_stop_lines(
            state,
            &flow.stop_station_ids,
            max_stop_rows,
        ));
    }

    let Some(station_id) = selected_station_id else {
        lines.push(Line::styled(
            "No Rail Station available.",
            theme::secondary(),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            details_area,
        );
        return;
    };

    if !flow.stop_station_ids.is_empty() {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![
        Span::styled("SELECTED  ", theme::table_header()),
        Span::styled(station_label(state, station_id), theme::focused_title()),
    ]));

    if let Some(selected_index) = flow
        .stop_station_ids
        .iter()
        .position(|selected_id| *selected_id == station_id)
    {
        lines.push(Line::styled(
            format!("Selected stop {} · Space to remove", selected_index + 1),
            theme::success(),
        ));
    } else if flow.stop_station_ids.is_empty() {
        lines.push(Line::styled(
            "Valid origin · Space to select",
            theme::success(),
        ));
    } else {
        let mut candidate = flow.stop_station_ids.clone();
        candidate.push(station_id);
        match service_path_for_stops(&state.region.rail_authority.rail_network, &candidate) {
            Ok(line_ids) => {
                let distance = distance_for_line_ids(state, &line_ids);
                lines.push(Line::styled(
                    format!("Valid next stop · {}", format::distance(distance)),
                    theme::success(),
                ));
            }
            Err(error) => lines.push(Line::styled(error.to_string(), theme::error())),
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        details_area,
    );
}

fn render_route_map(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &CreateServiceFlow,
    highlighted_station_id: Option<RailStationId>,
) {
    if area.width < 8 || area.height < 4 {
        return;
    }

    let network = &state.region.rail_authority.rail_network;
    let positions = mini_map_station_positions(state);
    if positions.is_empty() {
        frame.render_widget(
            Paragraph::new("No Rail Stations available.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let committed_line_ids = if flow.stop_station_ids.len() >= 2 {
        service_path_for_stops(network, &flow.stop_station_ids).unwrap_or_default()
    } else {
        Vec::new()
    };
    let preview_line_ids = highlighted_station_id
        .filter(|station_id| !flow.stop_station_ids.contains(station_id))
        .and_then(|station_id| {
            let mut candidate = flow.stop_station_ids.clone();
            candidate.push(station_id);
            (candidate.len() >= 2)
                .then(|| service_path_for_stops(network, &candidate).ok())
                .flatten()
        })
        .unwrap_or_default();

    let min_x = positions
        .values()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    let max_x = positions
        .values()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = positions
        .values()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min);
    let max_y = positions
        .values()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);
    let x_padding = ((max_x - min_x) * 0.12).max(4.0);
    let y_padding = ((max_y - min_y) * 0.12).max(2.0);

    let canvas = Canvas::default()
        .background_color(theme::PANEL)
        .marker(Marker::Braille)
        .x_bounds([min_x - x_padding, max_x + x_padding])
        .y_bounds([min_y - y_padding, max_y + y_padding])
        .paint(|context| {
            for rail_line in &network.rail_lines {
                if let Some((x1, y1, x2, y2)) = mini_map_line_endpoints(rail_line, &positions) {
                    context.draw(&CanvasLine::new(x1, y1, x2, y2, theme::SECONDARY));
                }
            }

            context.layer();
            for rail_line in &network.rail_lines {
                if committed_line_ids.contains(&rail_line.id) {
                    if let Some((x1, y1, x2, y2)) = mini_map_line_endpoints(rail_line, &positions) {
                        context.draw(&CanvasLine::new(x1, y1, x2, y2, theme::ACCENT));
                    }
                }
            }
            for rail_line in &network.rail_lines {
                if preview_line_ids.contains(&rail_line.id)
                    && !committed_line_ids.contains(&rail_line.id)
                {
                    if let Some((x1, y1, x2, y2)) = mini_map_line_endpoints(rail_line, &positions) {
                        context.draw(&CanvasLine::new(x1, y1, x2, y2, theme::SUCCESS));
                    }
                }
            }

            context.layer();
            for station in &network.rail_stations {
                let Some(&(x, y)) = positions.get(&station.id) else {
                    continue;
                };
                let stop_index = flow
                    .stop_station_ids
                    .iter()
                    .position(|station_id| *station_id == station.id);
                let highlighted = highlighted_station_id == Some(station.id);
                let (marker, style) = if highlighted {
                    let marker = stop_index
                        .map(|index| format!("◆{}", index + 1))
                        .unwrap_or_else(|| "◆".to_owned());
                    (marker, theme::warning())
                } else if let Some(index) = stop_index {
                    (format!("{}", index + 1), theme::focused_title())
                } else {
                    ("·".to_owned(), theme::secondary())
                };
                context.print(x, y, Line::styled(marker, style));
            }
        });
    frame.render_widget(canvas, area);
}

fn mini_map_station_positions(state: &GameState) -> BTreeMap<RailStationId, (f64, f64)> {
    let settlements = state
        .region
        .settlements
        .iter()
        .map(|settlement| (settlement.id, settlement))
        .collect::<BTreeMap<_, _>>();

    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .filter_map(|station| {
            let settlement = settlements.get(&station.settlement_id)?;
            // Match the operational Map's terminal-cell aspect correction and
            // invert Y because Canvas coordinates grow upward while terminal
            // rows grow downward.
            Some((
                station.id,
                (
                    f64::from(settlement.position.x),
                    -f64::from(settlement.position.y.div_euclid(2)),
                ),
            ))
        })
        .collect()
}

fn mini_map_line_endpoints(
    rail_line: &crate::model::RailLine,
    positions: &BTreeMap<RailStationId, (f64, f64)>,
) -> Option<(f64, f64, f64, f64)> {
    let &(x1, y1) = positions.get(&rail_line.first_station_id)?;
    let &(x2, y2) = positions.get(&rail_line.second_station_id)?;
    Some((x1, y1, x2, y2))
}

fn render_review(frame: &mut Frame, area: Rect, state: &GameState, flow: &CreateServiceFlow) {
    let line_ids = service_path_for_stops(
        &state.region.rail_authority.rail_network,
        &flow.stop_station_ids,
    )
    .unwrap_or_default();
    let distance = distance_for_line_ids(state, &line_ids);

    let [context_area, summary_area, stops_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Min(3),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(vec![
            step_line(2),
            Line::styled(
                if flow.editing_service_id.is_some() {
                    "Review Service Changes"
                } else {
                    "Review Passenger Service"
                },
                theme::title(),
            ),
        ])
        .style(theme::panel()),
        context_area,
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("ROUTE      ", theme::secondary()),
                Span::styled(
                    truncate_display(
                        &route_pattern_label(state, &flow.stop_station_ids, flow.direction_mode),
                        summary_area.width.saturating_sub(11) as usize,
                    ),
                    theme::focused_title(),
                ),
            ]),
            Line::from(vec![
                Span::styled("MODE       ", theme::secondary()),
                Span::styled(direction_mode_label(flow.direction_mode), theme::primary_value()),
            ]),
            Line::from(vec![
                Span::styled("DISTANCE   ", theme::secondary()),
                Span::styled(format::distance(distance), theme::primary_value()),
            ]),
            Line::from(vec![
                Span::styled("STOPS      ", theme::secondary()),
                Span::styled(
                    flow.stop_station_ids.len().to_string(),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(
                if flow.editing_service_id.is_some() {
                    "Service identity and existing train numbers are preserved where possible."
                } else {
                    match flow.direction_mode {
                        ServiceDirectionMode::BothDirections => {
                            "Bidirectional Service · dispatchable from either terminus."
                        }
                        ServiceDirectionMode::ForwardOnly => {
                            "One-way Service · dispatchable from its first stop only."
                        }
                    }
                },
                theme::secondary(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        summary_area,
    );

    let stop_lines = review_stop_lines(state, &flow.stop_station_ids, stops_area.height as usize);
    frame.render_widget(
        Paragraph::new(stop_lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        stops_area,
    );
}

fn route_pattern_label(
    state: &GameState,
    stop_station_ids: &[RailStationId],
    direction_mode: ServiceDirectionMode,
) -> String {
    let Some(origin_station_id) = stop_station_ids.first().copied() else {
        return "No stops selected yet".into();
    };
    let Some(destination_station_id) = stop_station_ids.last().copied() else {
        return station_label(state, origin_station_id);
    };
    if origin_station_id == destination_station_id {
        return station_label(state, origin_station_id);
    }
    let arrow = match direction_mode {
        ServiceDirectionMode::BothDirections => "↔",
        ServiceDirectionMode::ForwardOnly => "→",
    };
    format!(
        "{} {arrow} {}",
        station_label(state, origin_station_id),
        station_label(state, destination_station_id)
    )
}

fn direction_mode_label(direction_mode: ServiceDirectionMode) -> &'static str {
    match direction_mode {
        ServiceDirectionMode::BothDirections => "BOTH DIRECTIONS",
        ServiceDirectionMode::ForwardOnly => "ONE WAY",
    }
}

fn preview_stop_lines(
    state: &GameState,
    stop_station_ids: &[RailStationId],
    max_rows: usize,
) -> Vec<Line<'static>> {
    if stop_station_ids.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    if stop_station_ids.len() <= max_rows {
        return stop_station_ids
            .iter()
            .enumerate()
            .map(|(index, station_id)| {
                Line::from(vec![
                    Span::styled(format!("{}  ", index + 1), theme::secondary()),
                    Span::styled(station_label(state, *station_id), theme::primary_value()),
                ])
            })
            .collect();
    }

    if max_rows == 1 {
        return vec![Line::styled(
            format!("… {} stops selected", stop_station_ids.len()),
            theme::secondary(),
        )];
    }

    let visible_tail = max_rows.saturating_sub(1);
    let hidden = stop_station_ids.len().saturating_sub(visible_tail);
    let mut lines = vec![Line::styled(
        format!("… +{hidden} earlier stops"),
        theme::secondary(),
    )];
    lines.extend(
        stop_station_ids
            .iter()
            .enumerate()
            .skip(hidden)
            .map(|(index, station_id)| {
                Line::from(vec![
                    Span::styled(format!("{}  ", index + 1), theme::secondary()),
                    Span::styled(station_label(state, *station_id), theme::primary_value()),
                ])
            }),
    );
    lines
}

fn review_stop_lines(
    state: &GameState,
    stop_station_ids: &[RailStationId],
    max_lines: usize,
) -> Vec<Line<'static>> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut lines = vec![Line::styled("ORDERED STOPS", theme::table_header())];
    let row_capacity = max_lines.saturating_sub(1);
    if row_capacity == 0 || stop_station_ids.is_empty() {
        return lines;
    }

    let stop_line = |index: usize, station_id: RailStationId| {
        let suffix = if index == 0 {
            "  origin"
        } else if index + 1 == stop_station_ids.len() {
            "  destination"
        } else {
            ""
        };
        Line::from(vec![
            Span::styled(format!("{}  ", index + 1), theme::secondary()),
            Span::styled(station_label(state, station_id), theme::primary_value()),
            Span::styled(suffix, theme::secondary()),
        ])
    };

    if stop_station_ids.len() <= row_capacity {
        lines.extend(
            stop_station_ids
                .iter()
                .copied()
                .enumerate()
                .map(|(index, station_id)| stop_line(index, station_id)),
        );
        return lines;
    }

    if row_capacity == 1 {
        lines.push(Line::styled(
            format!("{} stops selected", stop_station_ids.len()),
            theme::secondary(),
        ));
        return lines;
    }

    lines.push(stop_line(0, stop_station_ids[0]));
    if row_capacity == 2 {
        let last = stop_station_ids.len() - 1;
        lines.push(stop_line(last, stop_station_ids[last]));
        return lines;
    }

    let middle_slots = row_capacity.saturating_sub(3);
    for (index, station_id) in stop_station_ids
        .iter()
        .copied()
        .enumerate()
        .skip(1)
        .take(middle_slots)
    {
        lines.push(stop_line(index, station_id));
    }
    let hidden_middle = stop_station_ids
        .len()
        .saturating_sub(2)
        .saturating_sub(middle_slots);
    if hidden_middle > 0 {
        lines.push(Line::styled(
            format!("… +{hidden_middle} intermediate stops"),
            theme::secondary(),
        ));
    }
    let last = stop_station_ids.len() - 1;
    lines.push(stop_line(last, stop_station_ids[last]));
    lines.truncate(max_lines);
    lines
}

fn step_line(active: u8) -> Line<'static> {
    let mut spans = Vec::new();
    for (step, label) in [(1, "STOPS"), (2, "REVIEW")] {
        if step > 1 {
            spans.push(Span::styled("  →  ", theme::secondary()));
        }
        let style = if step == active {
            theme::focused_title()
        } else if step < active {
            theme::success()
        } else {
            theme::secondary()
        };
        spans.push(Span::styled(format!("{step} {label}"), style));
    }
    Line::from(spans)
}

fn distance_for_line_ids(state: &GameState, line_ids: &[crate::model::RailLineId]) -> u64 {
    line_ids
        .iter()
        .filter_map(|line_id| {
            state
                .region
                .rail_authority
                .rail_network
                .rail_lines
                .iter()
                .find(|line| line.id == *line_id)
                .map(|line| line.distance.metres())
        })
        .sum()
}
