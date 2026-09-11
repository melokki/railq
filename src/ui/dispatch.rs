//! Keyboard state and text presentation for a Map Manual Dispatch.
//!
//! This module only keeps a proposed Train and destination. It previews a
//! Passenger Service on a cloned state, so the actual Service is not created
//! until the application boundary confirms the Manual Dispatch.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Gauge, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
    },
};

use crate::{
    model::{GameState, Money, RailStationId, TrainId, TrainStatus},
    sim::{
        economy::{JourneyQuote, quote_journey},
        services::find_or_create_service,
    },
    ui::theme,
};

/// The result of handling a key within the Map Manual Dispatch flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchFlowAction {
    /// The player is still reviewing or selecting the dispatch.
    Continue,
    /// The player abandoned the flow without authorising a Journey.
    Cancel,
    /// The application boundary must revalidate and authorise this dispatch.
    Confirm {
        train_id: TrainId,
        destination_station_id: RailStationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DispatchStep {
    SelectTrain {
        selected_train_id: Option<TrainId>,
        table_state: TableState,
        page_size: usize,
    },
    SelectDestination {
        train_id: TrainId,
        selected_destination_id: Option<RailStationId>,
        table_state: TableState,
        page_size: usize,
    },
    Confirm {
        train_id: TrainId,
        destination_station_id: RailStationId,
        quote: JourneyQuote,
        reuses_service: bool,
    },
}

/// Presentation state for one uncommitted Manual Dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchFlow {
    step: DispatchStep,
    preferred_station_id: Option<RailStationId>,
    rejection: Option<String>,
}

impl DispatchFlow {
    /// Starts selecting a READY Train. No game state changes at this point.
    pub fn start(state: &GameState) -> Result<Self, &'static str> {
        Self::start_at_station(state, None)
    }

    /// Starts at a focused Rail Station, preferring one of its READY Trains.
    /// The player can still deliberately choose any other available Fleet Train.
    pub fn start_at_station(
        state: &GameState,
        preferred_station_id: Option<RailStationId>,
    ) -> Result<Self, &'static str> {
        let train_ids = ready_train_ids(state);
        if train_ids.is_empty() {
            return Err(no_ready_train_reason(state));
        }
        let selected = preferred_station_id
            .and_then(|station_id| {
                train_ids
                    .iter()
                    .position(|train_id| ready_train_station(state, *train_id) == Some(station_id))
            })
            .unwrap_or(0);
        let mut table_state = TableState::default();
        table_state.select(Some(selected));
        Ok(Self {
            step: DispatchStep::SelectTrain {
                selected_train_id: Some(train_ids[selected]),
                table_state,
                page_size: 1,
            },
            preferred_station_id,
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> DispatchFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return DispatchFlowAction::Cancel;
        }

        match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                page_size,
            } => {
                let trains = ready_train_ids(state);
                if trains.is_empty() {
                    self.rejection = Some(no_ready_train_reason(state).into());
                    return DispatchFlowAction::Continue;
                }
                if synchronize_train_selection(selected_train_id, table_state, &trains) {
                    self.rejection = Some(
                        "The previously selected Train is no longer READY; choose an available Train."
                            .into(),
                    );
                }
                move_train_selection(
                    selected_train_id,
                    table_state,
                    &trains,
                    *page_size,
                    key.code,
                );
                if matches!(key.code, KeyCode::Enter) {
                    let Some(train_id) = *selected_train_id else {
                        if self.rejection.is_none() {
                            self.rejection = Some("Select a READY Train before continuing.".into());
                        }
                        return DispatchFlowAction::Continue;
                    };
                    if ready_train_station(state, train_id).is_none() {
                        self.rejection =
                            Some("That Train is no longer READY; select a Train again.".into());
                        return DispatchFlowAction::Continue;
                    }
                    match destination_step(state, train_id, None) {
                        Ok(step) => {
                            self.step = step;
                            self.rejection = None;
                        }
                        Err(error) => self.rejection = Some(error),
                    }
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::SelectDestination {
                train_id,
                selected_destination_id,
                table_state,
                page_size,
            } => {
                if matches!(key.code, KeyCode::Left | KeyCode::Backspace) {
                    let selected_train_id = *train_id;
                    self.step = train_selection_step(state, Some(selected_train_id));
                    self.rejection = None;
                    return DispatchFlowAction::Continue;
                }
                if ready_train_station(state, *train_id).is_none() {
                    self.rejection =
                        Some("That Train is no longer READY; Left or Backspace returns to Train selection.".into());
                    return DispatchFlowAction::Continue;
                }
                let destinations = destination_options(state, *train_id);
                if destinations.is_empty() {
                    self.rejection =
                        Some("No reachable Rail Station can be quoted from this origin.".into());
                    return DispatchFlowAction::Continue;
                }
                if synchronize_destination_selection(
                    selected_destination_id,
                    table_state,
                    &destinations,
                ) {
                    self.rejection = Some(
                        "The selected destination is no longer reachable; choose another route."
                            .into(),
                    );
                }
                move_destination_selection(
                    selected_destination_id,
                    table_state,
                    &destinations,
                    *page_size,
                    key.code,
                );
                if matches!(key.code, KeyCode::Enter) {
                    let Some(destination_station_id) = *selected_destination_id else {
                        self.rejection = Some(
                            "Select a reachable destination before continuing, or return to Train selection."
                                .into(),
                        );
                        return DispatchFlowAction::Continue;
                    };
                    if let Some(destination) = destinations
                        .iter()
                        .find(|destination| destination.station_id == destination_station_id)
                    {
                        let quote = destination.quote.clone();
                        let reuses_service = destination.reuses_service;
                        {
                            self.step = DispatchStep::Confirm {
                                train_id: *train_id,
                                destination_station_id,
                                quote,
                                reuses_service,
                            };
                            self.rejection = None;
                        }
                    } else {
                        self.rejection = Some(
                            "The selected destination changed; choose a reachable route again."
                                .into(),
                        );
                    }
                }
                DispatchFlowAction::Continue
            }
            DispatchStep::Confirm {
                train_id,
                destination_station_id,
                quote,
                reuses_service,
            } => {
                if matches!(key.code, KeyCode::Left | KeyCode::Backspace) {
                    match destination_step(state, *train_id, Some(*destination_station_id)) {
                        Ok(step) => {
                            self.step = step;
                            self.rejection = None;
                        }
                        Err(error) => self.rejection = Some(error),
                    }
                    DispatchFlowAction::Continue
                } else if matches!(key.code, KeyCode::Enter) {
                    match preview_quote(state, *train_id, *destination_station_id) {
                        Ok((current_quote, current_reuses_service))
                            if current_quote != *quote
                                || current_reuses_service != *reuses_service =>
                        {
                            *quote = current_quote;
                            *reuses_service = current_reuses_service;
                            self.rejection = Some(
                                "Journey quote updated from current conditions. Review the revised consequences and press Enter again."
                                    .into(),
                            );
                            DispatchFlowAction::Continue
                        }
                        Ok(_) => DispatchFlowAction::Confirm {
                            train_id: *train_id,
                            destination_station_id: *destination_station_id,
                        },
                        Err(error) => {
                            self.rejection = Some(format!(
                                "Journey quote could not be revalidated: {error}. Your choices are still available."
                            ));
                            DispatchFlowAction::Continue
                        }
                    }
                } else {
                    DispatchFlowAction::Continue
                }
            }
        }
    }

    /// Records an application-boundary rejection while keeping the proposal
    /// open so the concrete cause remains visible to the player.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the current keyboard step and the uncommitted Journey quote.
    pub fn render(&self, state: &GameState) -> String {
        let mut output = String::from("Manual Dispatch\n");
        match &self.step {
            DispatchStep::SelectTrain {
                selected_train_id, ..
            } => {
                output.push_str("Select a READY Train (Up/Down, Enter; Esc cancels):\n");
                for train_id in ready_train_ids(state) {
                    let marker = if Some(train_id) == *selected_train_id {
                        '>'
                    } else {
                        ' '
                    };
                    let Some(train) = state
                        .player_company
                        .fleet
                        .trains
                        .iter()
                        .find(|train| train.id == train_id)
                    else {
                        continue;
                    };
                    let location = ready_train_station(state, train_id)
                        .map(|station_id| station_label(state, station_id))
                        .unwrap_or("unknown Rail Station");
                    output.push_str(&format!(
                        " {marker} Train {} ({}) at {location}\n",
                        train.id.get(),
                        train.model_name
                    ));
                }
            }
            DispatchStep::SelectDestination {
                train_id,
                selected_destination_id,
                ..
            } => {
                let origin = ready_train_station(state, *train_id);
                output.push_str(&format!(
                    "Select a destination for Train {} from {} (Up/Down, Enter; Left/Backspace returns to Train; Esc cancels):\n",
                    train_id.get(),
                    origin
                        .map(|station_id| station_label(state, station_id))
                        .unwrap_or("unknown Rail Station")
                ));
                for destination in destination_options(state, *train_id) {
                    let marker = if Some(destination.station_id) == *selected_destination_id {
                        '>'
                    } else {
                        ' '
                    };
                    output.push_str(&format!(
                        " {marker} {} — {} waiting, {}, {}\n",
                        station_label(state, destination.station_id),
                        waiting_passengers(
                            state,
                            destination.quote.origin_station_id,
                            destination.station_id
                        ),
                        format_distance(destination.quote.distance.metres()),
                        format_path(state, &destination.quote),
                    ));
                }
            }
            DispatchStep::Confirm {
                quote,
                reuses_service,
                ..
            } => {
                output.push_str(&format!(
                    "{} Passenger Service: {} -> {} ({} Rail Lines)\n",
                    if *reuses_service { "Reuse" } else { "Create" },
                    station_label(state, quote.origin_station_id),
                    station_label(state, quote.destination_station_id),
                    quote.rail_line_path.len(),
                ));
                output.push_str(&format!(
                    "Directional Demand: {} Waiting Passengers; {} boarding / {} capacity\n",
                    waiting_passengers(
                        state,
                        quote.origin_station_id,
                        quote.destination_station_id
                    ),
                    quote.boarded_passengers,
                    train_capacity(state, quote.train_id),
                ));
                output.push_str(&format!(
                    "Route: {} | Duration: {} | Arrival revenue: {}\n",
                    format_path(state, quote),
                    format_duration(quote.duration.seconds()),
                    format_money(quote.operating_revenue),
                ));
                output.push_str(&format!(
                    "Infrastructure Access Fee: {} | Fuel Cost: {} | Paid-now total: {}\n",
                    format_money(quote.infrastructure_access_fee),
                    format_money(quote.fuel_cost),
                    format_money(quote.operating_cost),
                ));
                output.push_str(&format!(
                    "Estimated profit: {} | Company Funds after departure: {}\n",
                    format_money(quote.journey_profitability),
                    format_money(quote.cash_after_cost),
                ));
                if quote.boarded_passengers == 0 {
                    output.push_str("EMPTY REPOSITIONING: no arrival revenue is expected.\n");
                }
                if quote.cash_after_cost < Money::ZERO {
                    output.push_str(
                        "INSUFFICIENT FUNDS: paid-now total exceeds current Company Funds.\n",
                    );
                }
                output.push_str("Enter confirms Manual Dispatch (revalidated); Esc cancels.\n");
            }
        }
        if let Some(rejection) = &self.rejection {
            output.push_str(&format!("Departure rejected: {rejection}\n"));
        }
        output
    }

    /// Renders the stateful Manual Dispatch flow without changing the game.
    pub fn render_panel(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        let selection_changed = match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                ..
            } => {
                synchronize_train_selection(selected_train_id, table_state, &ready_train_ids(state))
            }
            DispatchStep::SelectDestination { .. } | DispatchStep::Confirm { .. } => false,
        };
        if selection_changed {
            self.rejection = Some(
                "The previously selected Train is no longer READY; choose an available Train."
                    .into(),
            );
        }

        match &mut self.step {
            DispatchStep::SelectTrain {
                selected_train_id,
                table_state,
                page_size,
            } => render_train_chooser(
                frame,
                area,
                TrainChooserContext {
                    state,
                    preferred_station_id: self.preferred_station_id,
                    selected_train_id: *selected_train_id,
                    rejection: self.rejection.as_deref(),
                },
                table_state,
                page_size,
            ),
            DispatchStep::SelectDestination {
                train_id,
                selected_destination_id,
                table_state,
                page_size,
            } => render_destination_chooser(
                frame,
                area,
                DestinationChooserContext {
                    state,
                    train_id: *train_id,
                    rejection: self.rejection.as_deref(),
                },
                selected_destination_id,
                table_state,
                page_size,
            ),
            DispatchStep::Confirm { .. } => {
                let DispatchStep::Confirm {
                    quote,
                    reuses_service,
                    ..
                } = &self.step
                else {
                    unreachable!("Manual Dispatch review requires a Journey quote");
                };
                render_quote_review(
                    frame,
                    area,
                    state,
                    quote,
                    *reuses_service,
                    self.rejection.as_deref(),
                );
            }
        }
    }
}

fn render_quote_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    quote: &JourneyQuote,
    reuses_service: bool,
    rejection: Option<&str>,
) {
    let block = dispatch_panel_block("Manual Dispatch · Journey quote", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let insufficient_funds = quote.cash_after_cost < Money::ZERO;
    let footer_rows = u16::from(insufficient_funds) + u16::from(rejection.is_some()) + 1;
    let [route_area, occupancy_area, terms_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(footer_rows),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    if reuses_service {
                        "Existing Passenger Service  "
                    } else {
                        "New Passenger Service preview  "
                    },
                    theme::focused_title(),
                ),
                Span::styled(
                    format!(
                        "{} → {}",
                        station_label(state, quote.origin_station_id),
                        station_label(state, quote.destination_station_id)
                    ),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(
                format!(
                    "Route: {} · {} · {} Rail Lines",
                    format_path(state, quote),
                    format_duration(quote.duration.seconds()),
                    quote.rail_line_path.len(),
                ),
                theme::secondary(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        route_area,
    );

    let capacity = train_capacity(state, quote.train_id);
    let occupancy_ratio = if capacity == 0 {
        0.0
    } else {
        f64::from(quote.boarded_passengers.min(capacity)) / f64::from(capacity)
    };
    frame.render_widget(
        Gauge::default()
            .block(dispatch_panel_block("Occupancy", false))
            .gauge_style(theme::focused_title())
            .ratio(occupancy_ratio)
            .label(format!(
                "{} boarded / {} capacity · {} Waiting Passengers",
                quote.boarded_passengers,
                capacity,
                waiting_passengers(state, quote.origin_station_id, quote.destination_station_id)
            )),
        occupancy_area,
    );

    let mut terms = vec![
        Line::styled("DEPARTURE · paid now", theme::warning()),
        money_pair_line(
            "Infrastructure Access Fee",
            quote.infrastructure_access_fee,
            "Fuel Cost",
            quote.fuel_cost,
        ),
        money_pair_line(
            "Paid-now total",
            quote.operating_cost,
            "Funds after departure",
            quote.cash_after_cost,
        ),
        Line::styled(
            "ARRIVAL · credited when the Journey arrives",
            theme::success(),
        ),
        money_pair_line(
            "Arrival revenue",
            quote.operating_revenue,
            "Estimated profit",
            quote.journey_profitability,
        ),
    ];
    if quote.boarded_passengers == 0 {
        terms.push(Line::styled(
            "EMPTY REPOSITIONING · no arrival revenue is expected.",
            theme::warning(),
        ));
    }
    frame.render_widget(
        Paragraph::new(terms)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        terms_area,
    );

    let mut footer = Vec::new();
    if insufficient_funds {
        footer.push(Line::styled(
            "INSUFFICIENT FUNDS · paid-now total exceeds current Company Funds.",
            theme::error(),
        ));
    }
    if let Some(rejection) = rejection {
        footer.push(Line::styled(
            format!("Departure review: {rejection}"),
            theme::error(),
        ));
    }
    footer.push(Line::styled(
        "Enter · revalidate and confirm   Left / Backspace · destination   Esc · cancel",
        theme::hint(),
    ));
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn money_pair_line(
    first_label: &str,
    first_value: Money,
    second_label: &str,
    second_value: Money,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{first_label}: "), theme::secondary()),
        Span::styled(format_money(first_value), theme::primary_value()),
        Span::styled(format!("  {second_label}: "), theme::secondary()),
        Span::styled(format_money(second_value), theme::primary_value()),
    ])
}

struct TrainChooserContext<'a> {
    state: &'a GameState,
    preferred_station_id: Option<RailStationId>,
    selected_train_id: Option<TrainId>,
    rejection: Option<&'a str>,
}

fn render_train_chooser(
    frame: &mut Frame,
    area: Rect,
    chooser: TrainChooserContext<'_>,
    table_state: &mut TableState,
    page_size: &mut usize,
) {
    let state = chooser.state;
    let trains = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .collect::<Vec<_>>();
    let station_context = chooser.preferred_station_id.map_or_else(
        || "Choose a READY Train from the available Fleet.".to_owned(),
        |station_id| {
            let station = station_label(state, station_id);
            if trains
                .iter()
                .any(|train| matches!(train.status, TrainStatus::Ready { at } if at == station_id))
            {
                format!("{station}: a READY Train here is preselected.")
            } else {
                format!("No READY Train at {station}; showing the available Fleet.")
            }
        },
    );
    let footer_rows = u16::from(chooser.rejection.is_some() || chooser.selected_train_id.is_none())
        .saturating_add(1);
    let [context_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("1 Train", theme::focused_title()),
            Span::styled(" → 2 Destination → 3 Review", theme::secondary()),
            Span::styled(format!("  {station_context}"), theme::secondary()),
        ]))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    *page_size = usize::from(table_area.height.saturating_sub(4)).max(1);
    let rows = trains
        .iter()
        .map(|train| {
            let TrainStatus::Ready { at } = train.status else {
                unreachable!("READY Train chooser only includes READY Trains");
            };
            Row::new([
                Cell::from(format!("Train {:02}", train.id.get())),
                Cell::from(train.model_name.clone()),
                Cell::from("READY"),
                Cell::from(format!("At {}", station_label(state, at))),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(36),
            Constraint::Length(9),
            Constraint::Percentage(44),
        ],
    )
    .header(
        Row::new(["Train", "Model", "Status", "Location"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .block(dispatch_panel_block(
        "Manual Dispatch · available Fleet",
        true,
    ))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("> ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, table_state);

    let controls = if area.width <= 80 {
        "↑↓ / J K · select   PgUp/Dn · scroll   Enter · next   Esc · cancel"
    } else {
        "↑↓ / J K · select   PageUp / PageDown · scroll   Enter · destination   Esc · cancel"
    };
    let mut footer = vec![Line::styled(controls, theme::hint())];
    if let Some(rejection) = chooser.rejection {
        footer.insert(
            0,
            Line::styled(format!("Dispatch unavailable: {rejection}"), theme::error()),
        );
    }
    if chooser.selected_train_id.is_none() && chooser.rejection.is_none() {
        footer.insert(
            0,
            Line::styled("Choose a READY Train to continue.", theme::warning()),
        );
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

#[derive(Clone, Debug)]
struct DestinationOption {
    station_id: RailStationId,
    quote: JourneyQuote,
    reuses_service: bool,
}

struct DestinationChooserContext<'a> {
    state: &'a GameState,
    train_id: TrainId,
    rejection: Option<&'a str>,
}

fn render_destination_chooser(
    frame: &mut Frame,
    area: Rect,
    chooser: DestinationChooserContext<'_>,
    selected_destination_id: &mut Option<RailStationId>,
    table_state: &mut TableState,
    page_size: &mut usize,
) {
    let state = chooser.state;
    let train_id = chooser.train_id;
    let Some(origin_station_id) = ready_train_station(state, train_id) else {
        render_destination_unavailable(
            frame,
            area,
            "That Train is no longer READY. Left or Backspace returns to Train selection.",
        );
        return;
    };
    let destinations = destination_options(state, train_id);
    if destinations.is_empty() {
        render_destination_unavailable(
            frame,
            area,
            "No reachable Rail Station can be quoted from this origin. Left or Backspace returns to Train selection.",
        );
        return;
    }

    synchronize_destination_selection(selected_destination_id, table_state, &destinations);
    let footer_rows = u16::from(chooser.rejection.is_some()).saturating_add(1);
    let [context_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("1 Train → ", theme::secondary()),
            Span::styled("2 Destination", theme::focused_title()),
            Span::styled(" → 3 Review", theme::secondary()),
            Span::styled(
                format!("  Origin: {}", station_label(state, origin_station_id)),
                theme::secondary(),
            ),
        ]))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        context_area,
    );

    let has_inspector = body_area.height >= 9;
    let wide = body_area.width >= 96 && has_inspector;
    let (table_area, inspector_area) = if wide {
        let [table_area, inspector_area] =
            Layout::horizontal([Constraint::Min(38), Constraint::Length(36)]).areas(body_area);
        (table_area, Some(inspector_area))
    } else if has_inspector {
        let [table_area, inspector_area] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).areas(body_area);
        (table_area, Some(inspector_area))
    } else {
        (body_area, None)
    };

    *page_size = usize::from(table_area.height.saturating_sub(4)).max(1);
    let rows = destinations
        .iter()
        .map(|destination| {
            let quote = &destination.quote;
            if wide {
                Row::new([
                    Cell::from(station_label(state, destination.station_id).to_owned()),
                    Cell::from(format!(
                        "{} waiting",
                        waiting_passengers(state, quote.origin_station_id, destination.station_id)
                    )),
                    Cell::from(format!("{} lines", quote.rail_line_path.len())),
                    Cell::from(format_distance(quote.distance.metres())),
                ])
            } else {
                Row::new([
                    Cell::from(station_label(state, destination.station_id).to_owned()),
                    Cell::from(format!(
                        "{} waiting",
                        waiting_passengers(state, quote.origin_station_id, destination.station_id)
                    )),
                    Cell::from(format_distance(quote.distance.metres())),
                ])
            }
        })
        .collect::<Vec<_>>();
    let widths = if wide {
        vec![
            Constraint::Percentage(38),
            Constraint::Percentage(24),
            Constraint::Percentage(18),
            Constraint::Percentage(20),
        ]
    } else {
        vec![
            Constraint::Percentage(48),
            Constraint::Percentage(28),
            Constraint::Percentage(24),
        ]
    };
    let headers = if wide {
        Row::new(["Destination", "Demand", "Path", "Distance"])
    } else {
        Row::new(["Destination", "Demand", "Distance"])
    };
    let table = Table::new(rows, widths)
        .header(headers.style(theme::table_header()).bottom_margin(1))
        .block(dispatch_panel_block(
            "Manual Dispatch · reachable destinations",
            true,
        ))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, table_state);

    if let Some(inspector_area) = inspector_area {
        let selected = selected_destination_id.and_then(|station_id| {
            destinations
                .iter()
                .find(|destination| destination.station_id == station_id)
        });
        render_route_inspector(frame, inspector_area, state, selected, wide);
    }

    let controls = if area.width <= 80 {
        "↑↓/J K · route  Enter · review  Left/Backspace · Train  Esc · cancel"
    } else {
        "↑↓ / J K · select route   PageUp / PageDown · scroll   Enter · review   Left / Backspace · Train   Esc · cancel"
    };
    let mut footer = vec![Line::styled(controls, theme::hint())];
    if let Some(rejection) = chooser.rejection {
        footer.insert(
            0,
            Line::styled(format!("Dispatch unavailable: {rejection}"), theme::error()),
        );
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn render_destination_unavailable(frame: &mut Frame, area: Rect, reason: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("1 Train → 2 Destination → 3 Review", theme::focused_title()),
            Line::styled(reason, theme::error()),
            Line::styled("Left / Backspace · Train   Esc · cancel", theme::hint()),
        ])
        .block(dispatch_panel_block(
            "Manual Dispatch · destination unavailable",
            true,
        ))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_route_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    destination: Option<&DestinationOption>,
    wide: bool,
) {
    let Some(destination) = destination else {
        frame.render_widget(
            Paragraph::new("Select a reachable destination to inspect its route.")
                .block(dispatch_panel_block("Route inspector", true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    let quote = &destination.quote;
    let demand = waiting_passengers(state, quote.origin_station_id, destination.station_id);
    let service = if destination.reuses_service {
        "Existing Passenger Service"
    } else {
        "New Passenger Service preview"
    };
    let lines = if wide {
        vec![
            Line::from(vec![
                Span::styled("Origin  ", theme::secondary()),
                Span::styled(
                    station_label(state, quote.origin_station_id),
                    theme::primary_value(),
                ),
            ]),
            Line::from(vec![
                Span::styled("Destination  ", theme::secondary()),
                Span::styled(
                    station_label(state, destination.station_id),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(
                format!("Directional demand  {demand} waiting"),
                theme::primary_value(),
            ),
            Line::styled(
                format!("Path  {}", format_path(state, quote)),
                theme::secondary(),
            ),
            Line::styled(
                format!(
                    "Distance  {} · {}",
                    format_distance(quote.distance.metres()),
                    service
                ),
                theme::secondary(),
            ),
        ]
    } else {
        vec![
            Line::styled(
                format!(
                    "{} → {} · {demand} waiting",
                    station_label(state, quote.origin_station_id),
                    station_label(state, destination.station_id)
                ),
                theme::primary_value(),
            ),
            Line::styled(
                format!("Path: {}", format_path(state, quote)),
                theme::secondary(),
            ),
            Line::styled(
                format!("{} · {service}", format_distance(quote.distance.metres())),
                theme::secondary(),
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(dispatch_panel_block("Route inspector", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn dispatch_panel_block(title: &str, focused: bool) -> Block<'_> {
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

fn ready_train_ids(state: &GameState) -> Vec<TrainId> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter_map(|train| match train.status {
            TrainStatus::Ready { .. } => Some(train.id),
            TrainStatus::Travelling { .. } => None,
        })
        .collect()
}

fn no_ready_train_reason(state: &GameState) -> &'static str {
    if state.player_company.fleet.trains.is_empty() {
        "No READY Train in the Fleet. Press B to buy a Train."
    } else {
        "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then dispatch from Map."
    }
}

/// Reconciles table focus by stable Train ID. A missing selected Train is
/// deliberately cleared rather than replaced with a different Fleet entry.
fn synchronize_train_selection(
    selected_train_id: &mut Option<TrainId>,
    table_state: &mut TableState,
    train_ids: &[TrainId],
) -> bool {
    let Some(train_id) = *selected_train_id else {
        table_state.select(None);
        return false;
    };
    if let Some(index) = train_ids
        .iter()
        .position(|candidate| *candidate == train_id)
    {
        table_state.select(Some(index));
        false
    } else {
        *selected_train_id = None;
        table_state.select(None);
        *table_state.offset_mut() = 0;
        true
    }
}

fn move_train_selection(
    selected_train_id: &mut Option<TrainId>,
    table_state: &mut TableState,
    train_ids: &[TrainId],
    page_size: usize,
    key: KeyCode,
) {
    let current = selected_train_id.and_then(|train_id| {
        train_ids
            .iter()
            .position(|candidate| *candidate == train_id)
    });
    let last = train_ids.len().saturating_sub(1);
    let next = match key {
        KeyCode::Up | KeyCode::Char('k') => current.map_or(0, |index| index.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            current.map_or(0, |index| index.saturating_add(1).min(last))
        }
        KeyCode::PageUp => current.map_or(0, |index| index.saturating_sub(page_size.max(1))),
        KeyCode::PageDown => {
            current.map_or(0, |index| index.saturating_add(page_size.max(1)).min(last))
        }
        _ => return,
    };
    *selected_train_id = train_ids.get(next).copied();
    table_state.select(Some(next));
}

fn ready_train_station(state: &GameState, train_id: TrainId) -> Option<RailStationId> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .and_then(|train| match train.status {
            TrainStatus::Ready { at } => Some(at),
            TrainStatus::Travelling { .. } => None,
        })
}

fn train_capacity(state: &GameState, train_id: TrainId) -> u32 {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or(0, |train| train.passenger_capacity.passengers())
}

fn train_selection_step(state: &GameState, selected_train_id: Option<TrainId>) -> DispatchStep {
    let train_ids = ready_train_ids(state);
    let selected = selected_train_id.and_then(|train_id| {
        train_ids
            .iter()
            .position(|candidate| *candidate == train_id)
    });
    let mut table_state = TableState::default();
    table_state.select(selected);
    DispatchStep::SelectTrain {
        selected_train_id: selected.map(|index| train_ids[index]),
        table_state,
        page_size: 1,
    }
}

fn destination_step(
    state: &GameState,
    train_id: TrainId,
    selected_destination_id: Option<RailStationId>,
) -> Result<DispatchStep, String> {
    let destinations = destination_options(state, train_id);
    if destinations.is_empty() {
        return Err("No reachable Rail Station can be quoted from this origin.".into());
    }
    let selected = selected_destination_id
        .and_then(|station_id| {
            destinations
                .iter()
                .position(|destination| destination.station_id == station_id)
        })
        .unwrap_or(0);
    let mut table_state = TableState::default();
    table_state.select(Some(selected));
    Ok(DispatchStep::SelectDestination {
        train_id,
        selected_destination_id: Some(destinations[selected].station_id),
        table_state,
        page_size: 1,
    })
}

fn destination_options(state: &GameState, train_id: TrainId) -> Vec<DestinationOption> {
    let Some(origin_station_id) = ready_train_station(state, train_id) else {
        return Vec::new();
    };
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .filter(|station_id| *station_id != origin_station_id)
        .filter_map(|station_id| {
            preview_quote(state, train_id, station_id)
                .ok()
                .map(|(quote, reuses_service)| DestinationOption {
                    station_id,
                    quote,
                    reuses_service,
                })
        })
        .collect()
}

fn preview_quote(
    state: &GameState,
    train_id: TrainId,
    destination_station_id: RailStationId,
) -> Result<(JourneyQuote, bool), String> {
    let origin_station_id = ready_train_station(state, train_id)
        .ok_or_else(|| "That Train is no longer READY; select a Train again.".to_owned())?;
    let reuses_service = state
        .player_company
        .passenger_services
        .iter()
        .any(|service| {
            (service.first_station_id == origin_station_id
                && service.second_station_id == destination_station_id)
                || (service.first_station_id == destination_station_id
                    && service.second_station_id == origin_station_id)
        });
    let mut preview_state = state.clone();
    let service_id = find_or_create_service(
        &mut preview_state,
        origin_station_id,
        destination_station_id,
    )
    .map_err(|error| error.to_string())?;
    quote_journey(&preview_state, train_id, service_id)
        .map_err(|error| error.to_string())
        .map(|quote| (quote, reuses_service))
}

fn synchronize_destination_selection(
    selected_destination_id: &mut Option<RailStationId>,
    table_state: &mut TableState,
    destinations: &[DestinationOption],
) -> bool {
    let Some(destination_id) = *selected_destination_id else {
        table_state.select(None);
        return false;
    };
    if let Some(index) = destinations
        .iter()
        .position(|destination| destination.station_id == destination_id)
    {
        table_state.select(Some(index));
        false
    } else {
        *selected_destination_id = None;
        table_state.select(None);
        *table_state.offset_mut() = 0;
        true
    }
}

fn move_destination_selection(
    selected_destination_id: &mut Option<RailStationId>,
    table_state: &mut TableState,
    destinations: &[DestinationOption],
    page_size: usize,
    key: KeyCode,
) {
    let current = selected_destination_id.and_then(|destination_id| {
        destinations
            .iter()
            .position(|destination| destination.station_id == destination_id)
    });
    let last = destinations.len().saturating_sub(1);
    let next = match key {
        KeyCode::Up | KeyCode::Char('k') => current.map_or(0, |index| index.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            current.map_or(0, |index| index.saturating_add(1).min(last))
        }
        KeyCode::PageUp => current.map_or(0, |index| index.saturating_sub(page_size.max(1))),
        KeyCode::PageDown => {
            current.map_or(0, |index| index.saturating_add(page_size.max(1)).min(last))
        }
        _ => return,
    };
    *selected_destination_id = destinations
        .get(next)
        .map(|destination| destination.station_id);
    table_state.select(Some(next));
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

fn waiting_passengers(
    state: &GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> u32 {
    state
        .origin_destination_demand
        .iter()
        .find(|demand| {
            demand.origin_station_id == origin_station_id
                && demand.destination_station_id == destination_station_id
        })
        .map_or(0, |demand| demand.waiting_passengers)
}

fn format_path(state: &GameState, quote: &JourneyQuote) -> String {
    let mut stations = vec![quote.origin_station_id];
    let mut current_station_id = quote.origin_station_id;
    for rail_line_id in &quote.rail_line_path {
        let Some(rail_line) = state
            .region
            .rail_authority
            .rail_network
            .rail_lines
            .iter()
            .find(|rail_line| rail_line.id == *rail_line_id)
        else {
            return "unknown Rail Line path".into();
        };
        let next_station_id = if rail_line.first_station_id == current_station_id {
            rail_line.second_station_id
        } else if rail_line.second_station_id == current_station_id {
            rail_line.first_station_id
        } else {
            return "invalid Rail Line path".into();
        };
        stations.push(next_station_id);
        current_station_id = next_station_id;
    }
    stations
        .into_iter()
        .map(|station_id| station_label(state, station_id))
        .collect::<Vec<_>>()
        .join(" → ")
}

fn format_distance(metres: u64) -> String {
    format!("{}.{:01} km", metres / 1_000, (metres % 1_000) / 100)
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{RailStationId, TrainStatus, UtcSeconds},
        sim::{fleet::purchase_train, services::find_or_create_service, world::create_new_game},
    };

    use super::{DispatchFlow, DispatchFlowAction};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn game_with_ready_train(
        at: RailStationId,
    ) -> (crate::model::GameState, crate::model::TrainId) {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, at).unwrap();
        (state, train_id)
    }

    #[test]
    fn multi_line_trip_is_keyboard_reachable_without_creating_a_service() {
        let (state, train_id) = game_with_ready_train(RailStationId::new(1));
        let mut flow = DispatchFlow::start(&state).unwrap();

        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Down), &state),
            DispatchFlowAction::Continue
        );
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Continue
        );

        let rendered = flow.render(&state);
        assert!(rendered.contains("2 Rail Lines"));
        assert!(rendered.contains("Directional Demand:"));
        assert!(rendered.contains("Waiting Passengers"));
        assert!(state.player_company.passenger_services.is_empty());
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                destination_station_id: RailStationId::new(3),
            }
        );
    }

    #[test]
    fn return_trip_is_keyboard_reachable_and_reuses_the_service_preview() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(3));
        find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(3)).unwrap();
        let mut flow = DispatchFlow::start(&state).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);

        let rendered = flow.render(&state);
        assert!(rendered.contains("Reuse Passenger Service"));
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            DispatchFlowAction::Confirm {
                train_id,
                destination_station_id: RailStationId::new(1),
            }
        );
    }

    #[test]
    fn cancel_leaves_every_domain_collection_unchanged() {
        let (state, _) = game_with_ready_train(RailStationId::new(1));
        let before = state.clone();
        let mut flow = DispatchFlow::start(&state).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Down), &state);
        flow.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            flow.handle_key(key(KeyCode::Esc), &state),
            DispatchFlowAction::Cancel
        );

        assert_eq!(state, before);
    }

    #[test]
    fn rejection_stays_visible_with_its_concrete_cause() {
        let (state, _) = game_with_ready_train(RailStationId::new(1));
        let mut flow = DispatchFlow::start(&state).unwrap();
        flow.reject("Company Funds of 1 cents cannot cover Journey departure costs of 2 cents");

        assert!(
            flow.render(&state)
                .contains("Departure rejected: Company Funds")
        );
    }

    #[test]
    fn preview_does_not_treat_a_travelling_train_as_ready() {
        let (mut state, train_id) = game_with_ready_train(RailStationId::new(1));
        state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
            journey_id: crate::model::JourneyId::new(1),
        };
        assert_eq!(
            DispatchFlow::start(&state),
            Err(
                "No READY Train: all Fleet Trains are TRAVELLING. Wait for an arrival, then dispatch from Map."
            )
        );
        assert_eq!(train_id.get(), 1);
    }
}
