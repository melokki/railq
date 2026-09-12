//! Keyboard state and text presentation for buying catalogue Trains.
//!
//! A proposed purchase remains presentation-only until the Player Company
//! explicitly confirms it. The application boundary then revalidates the
//! catalogue entry, selected Rail Station, and Company Funds before saving.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{
        Block, Cell, HighlightSpacing, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Wrap,
    },
};

use crate::{
    catalog::{TrainModel, train_catalogue},
    model::{GameState, Money, RailLine, RailStationId},
    ui::theme,
};

/// Persistent catalogue focus. Catalogue records are saved with a game, so an
/// index is stable for the duration of this presentation-only purchase path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogueSelection {
    selected: usize,
    table_state: TableState,
    page_size: usize,
}

impl CatalogueSelection {
    /// Returns the currently focused catalogue record, if one remains available.
    pub fn selected_catalogue_index(&mut self, state: &GameState) -> Option<usize> {
        self.synchronize(state);
        (!train_catalogue().models().is_empty()).then_some(self.selected)
    }

    /// Moves the focused catalogue record without changing the game state.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let catalogue_len = train_catalogue().models().len();
        if catalogue_len == 0 {
            return;
        }
        self.selected = match key {
            KeyCode::Up | KeyCode::Char('k' | 'K') => self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j' | 'J') => self
                .selected
                .saturating_add(1)
                .min(catalogue_len.saturating_sub(1)),
            KeyCode::PageUp => self.selected.saturating_sub(self.page_size.max(1)),
            KeyCode::PageDown => self
                .selected
                .saturating_add(self.page_size.max(1))
                .min(catalogue_len.saturating_sub(1)),
            _ => self.selected,
        };
        self.table_state.select(Some(self.selected));
    }

    fn synchronize(&mut self, _state: &GameState) {
        let catalogue_len = train_catalogue().models().len();
        if catalogue_len == 0 {
            self.selected = 0;
            self.table_state.select(None);
            *self.table_state.offset_mut() = 0;
        } else {
            self.selected = self.selected.min(catalogue_len.saturating_sub(1));
            self.table_state.select(Some(self.selected));
        }
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
}

/// The result of handling a key within the Buy Trains flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketFlowAction {
    /// The player is still reviewing a proposed purchase.
    Continue,
    /// The player abandoned the proposed purchase.
    Cancel,
    /// The player returned from delivery selection to the catalogue.
    ReturnToCatalogue,
    /// The player returned from purchase review to delivery selection.
    ReturnToDelivery,
    /// The application boundary must revalidate and purchase the Train.
    Confirm {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MarketStep {
    SelectDelivery {
        catalogue_index: usize,
        selected_delivery_station_id: Option<RailStationId>,
        list_state: ListState,
        page_size: usize,
    },
    Confirm {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
}

/// Presentation state for one uncommitted catalogue Train purchase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketFlow {
    step: MarketStep,
    rejection: Option<String>,
}

impl MarketFlow {
    /// Returns whether this flow currently owns delivery Rail Station input.
    pub fn is_selecting_delivery(&self) -> bool {
        matches!(self.step, MarketStep::SelectDelivery { .. })
    }

    /// Starts delivery selection for a focused catalogue Train without changing
    /// the Fleet or Company Funds.
    pub fn start(state: &GameState, catalogue_index: usize) -> Result<Self, &'static str> {
        if train_catalogue().models().is_empty() {
            return Err("No diesel Train is available in the catalogue.");
        }
        if train_catalogue().models()
            .get(catalogue_index)
            .is_none()
        {
            return Err("The selected catalogue Train is no longer available.");
        }
        if delivery_station_ids(state).is_empty() {
            return Err("No connected Rail Station is available for delivery.");
        }
        let mut list_state = ListState::default();
        let selected_delivery_station_id = delivery_station_ids(state).first().copied();
        list_state.select(selected_delivery_station_id.map(|_| 0));
        Ok(Self {
            step: MarketStep::SelectDelivery {
                catalogue_index,
                selected_delivery_station_id,
                list_state,
                page_size: 1,
            },
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> MarketFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return MarketFlowAction::Cancel;
        }

        match &mut self.step {
            MarketStep::SelectDelivery {
                catalogue_index,
                selected_delivery_station_id,
                list_state,
                page_size,
            } => {
                if matches!(key.code, KeyCode::Left | KeyCode::Backspace) {
                    return MarketFlowAction::ReturnToCatalogue;
                }
                let stations = delivery_station_ids(state);
                if stations.is_empty() {
                    self.rejection =
                        Some("No connected Rail Station is available for delivery.".into());
                    return MarketFlowAction::Continue;
                }
                if train_catalogue().models()
                    .get(*catalogue_index)
                    .is_none()
                {
                    self.rejection = Some(
                        "The selected catalogue Train is no longer available; return to the catalogue and choose a current model."
                            .into(),
                    );
                    return MarketFlowAction::Continue;
                }
                if synchronize_delivery_selection(
                    selected_delivery_station_id,
                    list_state,
                    &stations,
                ) {
                    self.rejection = Some(
                        "The previously selected delivery Rail Station is no longer available; choose a current Rail Station."
                            .into(),
                    );
                    return MarketFlowAction::Continue;
                }
                move_delivery_selection(
                    selected_delivery_station_id,
                    list_state,
                    &stations,
                    *page_size,
                    key.code,
                );
                if matches!(key.code, KeyCode::Enter) {
                    if let Some(delivery_station_id) = *selected_delivery_station_id {
                        self.step = MarketStep::Confirm {
                            catalogue_index: *catalogue_index,
                            delivery_station_id,
                        };
                        self.rejection = None;
                    } else {
                        self.rejection = Some(
                            "Select a current delivery Rail Station before review, or return to the catalogue."
                                .into(),
                        );
                    }
                }
                MarketFlowAction::Continue
            }
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } if matches!(key.code, KeyCode::Left | KeyCode::Backspace) => {
                let catalogue_index = *catalogue_index;
                let delivery_station_id = *delivery_station_id;
                let stations = delivery_station_ids(state);
                let selected_index = stations
                    .iter()
                    .position(|station_id| *station_id == delivery_station_id);
                let mut list_state = ListState::default();
                list_state.select(selected_index);
                self.step = MarketStep::SelectDelivery {
                    catalogue_index,
                    selected_delivery_station_id: selected_index.map(|_| delivery_station_id),
                    list_state,
                    page_size: 1,
                };
                self.rejection = None;
                MarketFlowAction::ReturnToDelivery
            }
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } if matches!(key.code, KeyCode::Enter) => MarketFlowAction::Confirm {
                catalogue_index: *catalogue_index,
                delivery_station_id: *delivery_station_id,
            },
            MarketStep::Confirm { .. } => MarketFlowAction::Continue,
        }
    }

    /// Records an application-boundary rejection while keeping the proposal visible.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the stateful delivery chooser while keeping its selected Rail
    /// Station visible as the list scrolls.
    pub fn render_panel(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        match &mut self.step {
            MarketStep::SelectDelivery {
                catalogue_index,
                selected_delivery_station_id,
                list_state,
                page_size,
            } => render_delivery_chooser(
                frame,
                area,
                DeliveryChooserContext {
                    state,
                    catalogue_index: *catalogue_index,
                    rejection: self.rejection.as_deref(),
                },
                selected_delivery_station_id,
                list_state,
                page_size,
            ),
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } => render_purchase_review(
                frame,
                area,
                state,
                *catalogue_index,
                *delivery_station_id,
                self.rejection.as_deref(),
            ),
        }
    }

    /// Renders the proposed purchase and its current-step instructions.
    pub fn render(&self, state: &GameState) -> String {
        let selected_catalogue_index = match &self.step {
            MarketStep::SelectDelivery {
                catalogue_index, ..
            }
            | MarketStep::Confirm {
                catalogue_index, ..
            } => *catalogue_index,
        };
        let mut output = render_selected(state, selected_catalogue_index);

        match &self.step {
            MarketStep::SelectDelivery {
                catalogue_index: _,
                selected_delivery_station_id,
                ..
            } => {
                writeln!(
                    output,
                    "Select a delivery Rail Station (Up/Down, Enter; Esc cancels):"
                )
                .expect("writing to a String cannot fail");
                for station_id in delivery_station_ids(state) {
                    let marker = if Some(station_id) == *selected_delivery_station_id {
                        '>'
                    } else {
                        ' '
                    };
                    writeln!(output, " {marker} {}", station_label(state, station_id))
                        .expect("writing to a String cannot fail");
                }
            }
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } => {
                let train = train_catalogue().models().get(*catalogue_index);
                if let Some(train) = train {
                    render_purchase_review_text(&mut output, state, train, *delivery_station_id);
                }
                writeln!(
                    output,
                    "Enter confirms purchase (revalidated); Left / Backspace returns to delivery; Esc cancels."
                )
                .expect("writing to a String cannot fail");
            }
        }
        if let Some(rejection) = &self.rejection {
            writeln!(output, "Purchase rejected: {rejection}")
                .expect("writing to a String cannot fail");
        }
        output
    }
}

fn render_purchase_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
    rejection: Option<&str>,
) {
    let Some(train) = train_catalogue().models().get(catalogue_index) else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "1 Train → 2 Delivery Rail Station → 3 Review",
                    theme::focused_title(),
                ),
                Line::styled(
                    "The selected catalogue Train is no longer available. Return to the catalogue and choose a current model.",
                    theme::error(),
                ),
                Line::styled("Left / Backspace · delivery   Esc · cancel", theme::hint()),
            ])
            .block(panel_block("Train Market · purchase review", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };

    let footer_rows = 2_u16.saturating_add(u16::from(rejection.is_some()));
    let [step_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "1 Train → 2 Delivery Rail Station → 3 Review",
            theme::focused_title(),
        ))
        .style(theme::panel()),
        step_area,
    );

    if body_area.width >= 96 && body_area.height >= 10 {
        let [purchase_area, reserve_area] =
            Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
                .spacing(1)
                .areas(body_area);
        frame.render_widget(
            Paragraph::new(purchase_review_lines(state, train, delivery_station_id))
                .block(panel_block("Purchase", true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            purchase_area,
        );
        frame.render_widget(
            Paragraph::new(sample_reserve_lines(state, train))
                .block(panel_block("Sample departure reserve", false))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            reserve_area,
        );
    } else if body_area.height >= 16 {
        let mut lines = purchase_review_lines(state, train, delivery_station_id);
        lines.push(Line::from(""));
        lines.extend(sample_reserve_lines(state, train));
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel_block("Train Market · purchase review", true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            body_area,
        );
    } else {
        frame.render_widget(
            Paragraph::new(compact_purchase_review_lines(
                state,
                train,
                delivery_station_id,
            ))
            .block(panel_block("Train Market · purchase review", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            body_area,
        );
    }

    let mut footer = Vec::new();
    if low_reserve(state, train) {
        footer.push(Line::styled(
            "LOW RESERVE · funds after purchase cannot cover this sample departure cost.",
            theme::warning(),
        ));
    } else {
        footer.push(Line::styled(
            "Reserve check · funds after purchase cover this sample departure cost.",
            theme::success(),
        ));
    }
    if let Some(rejection) = rejection {
        footer.push(Line::styled(
            format!("Purchase rejected: {rejection}"),
            theme::error(),
        ));
    }
    footer.push(Line::styled(
        "Enter · confirm purchase (revalidated)   Left / Backspace · delivery   Esc · cancel",
        theme::hint(),
    ));
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn purchase_review_lines(
    state: &GameState,
    train: &TrainModel,
    delivery_station_id: RailStationId,
) -> Vec<Line<'static>> {
    vec![
        Line::styled(train.name().to_owned(), theme::focused_title()),
        Line::from(format!(
            "Delivery Rail Station: {}",
            station_label(state, delivery_station_id)
        )),
        Line::from("Delivery fee: $0.00"),
        Line::from(""),
        Line::from(format!("Price: {}", format_money(train.purchase_price()))),
        Line::from(format!(
            "Company Funds before purchase: {}",
            format_money(state.player_company.funds)
        )),
        Line::styled(
            format!(
                "Company Funds after purchase: {}",
                funds_after_purchase(state, train)
            ),
            theme::primary_value(),
        ),
    ]
}

fn sample_reserve_lines(
    state: &GameState,
    train: &TrainModel,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        "Sample Rail Line · reserve example only",
        theme::secondary(),
    )];
    if let Some(sample) = sample_trip(state, train) {
        lines.extend([
            Line::from(format!("Route: {} · {}", sample.route, sample.distance)),
            Line::from(format!(
                "Infrastructure Access Fee: {}",
                format_money(sample.access_fee)
            )),
            Line::from(format!("Fuel Cost: {}", format_money(sample.fuel_cost))),
            Line::styled(
                format!(
                    "Sample departure cost: {}",
                    format_money(sample.departure_cost)
                ),
                theme::primary_value(),
            ),
        ]);
    } else {
        lines.push(Line::styled(
            "Sample departure cost is unavailable for the current Rail Network.",
            theme::error(),
        ));
    }
    lines.push(Line::from(
        "This is one Rail Line example, not a planned Passenger Service or required Journey.",
    ));
    lines
}

fn compact_purchase_review_lines(
    state: &GameState,
    train: &TrainModel,
    delivery_station_id: RailStationId,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(format!("Model: {}", train.name()), theme::focused_title()),
        Line::from(format!(
            "Delivery Rail Station: {}",
            station_label(state, delivery_station_id)
        )),
        Line::from(format!("Price: {}", format_money(train.purchase_price()))),
        Line::from(format!(
            "Company Funds before purchase: {}",
            format_money(state.player_company.funds)
        )),
        Line::styled(
            format!(
                "Company Funds after purchase: {}",
                funds_after_purchase(state, train)
            ),
            theme::primary_value(),
        ),
    ];
    if let Some(sample) = sample_trip(state, train) {
        lines.push(Line::from(format!("Sample Rail Line: {}", sample.route)));
        lines.push(Line::styled(
            format!(
                "Sample departure cost: {}",
                format_money(sample.departure_cost)
            ),
            theme::primary_value(),
        ));
    } else {
        lines.push(Line::styled(
            "Sample departure cost unavailable.",
            theme::error(),
        ));
    }
    lines.push(Line::from(
        "Sample only; not a planned Passenger Service or required Journey.",
    ));
    lines
}

fn funds_after_purchase(state: &GameState, train: &TrainModel) -> String {
    match state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    {
        Ok(remaining) => format_money(remaining),
        Err(_) => "unavailable".into(),
    }
}

fn render_purchase_review_text(
    output: &mut String,
    state: &GameState,
    train: &TrainModel,
    delivery_station_id: RailStationId,
) {
    writeln!(output, "Purchase review").expect("writing to a String cannot fail");
    writeln!(output, "Model: {}", train.name()).expect("writing to a String cannot fail");
    writeln!(
        output,
        "Delivery Rail Station: {}",
        station_label(state, delivery_station_id)
    )
    .expect("writing to a String cannot fail");
    writeln!(output, "Price: {}", format_money(train.purchase_price()))
        .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Company Funds before purchase: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Company Funds after purchase: {}",
        funds_after_purchase(state, train)
    )
    .expect("writing to a String cannot fail");
    if let Some(sample) = sample_trip(state, train) {
        writeln!(
            output,
            "Sample Rail Line: {} ({})",
            sample.route, sample.distance
        )
        .expect("writing to a String cannot fail");
        writeln!(
            output,
            "Sample departure cost: Infrastructure Access Fee {} + Fuel Cost {} = {}",
            format_money(sample.access_fee),
            format_money(sample.fuel_cost),
            format_money(sample.departure_cost),
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        output,
        "Reserve example only: it is not a planned Passenger Service or required Journey."
    )
    .expect("writing to a String cannot fail");
    if low_reserve(state, train) {
        writeln!(
            output,
            "LOW RESERVE: funds after purchase cannot cover this sample departure cost."
        )
        .expect("writing to a String cannot fail");
    }
}

/// Renders the diesel Train catalogue before a purchase is started.
pub fn render(state: &GameState) -> String {
    let mut output = String::from("Market\n");
    for (index, train) in train_catalogue().models().iter().enumerate() {
        render_catalogue_train(&mut output, state, index, train);
    }
    if let Some(train) = train_catalogue().models().first() {
        writeln!(output, "Selected Train: {}", train.name())
            .expect("writing to a String cannot fail");
        render_purchase_implications(&mut output, state, train);
    } else {
        writeln!(output, "No diesel Train is available in the catalogue.")
            .expect("writing to a String cannot fail");
    }
    writeln!(
        output,
        "Enter starts a purchase; Esc is available after selection starts."
    )
    .expect("writing to a String cannot fail");
    output
}

/// Renders the selectable catalogue and focused model inspector.
///
/// The catalogue prioritises the purchase decision: price and remaining
/// Company Funds stay visible in the table, while the inspector carries the
/// selected Train's operating details and reserve implications.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut CatalogueSelection,
) {
    selection.synchronize(state);
    let catalogue = train_catalogue().models();
    if catalogue.is_empty() {
        frame.render_widget(
            Paragraph::new("No diesel Train is available in the catalogue.")
                .block(panel_block("Train Market", true))
                .style(theme::panel()),
            area,
        );
        return;
    }

    let wide = area.width >= 96 && area.height >= 14;
    let [table_area, inspector_area] = if wide {
        Layout::horizontal([Constraint::Min(58), Constraint::Length(38)])
            .spacing(1)
            .areas(area)
    } else {
        Layout::vertical([Constraint::Length(8), Constraint::Min(6)]).areas(area)
    };
    selection.set_page_size(usize::from(table_area.height.saturating_sub(4)).max(1));
    selection.synchronize(state);

    let rows = catalogue
        .iter()
        .map(|train| {
            if wide {
                Row::new([
                    Cell::from(train.name().to_owned()),
                    Cell::from(format_money(train.purchase_price())),
                    Cell::from(funds_after_purchase_display(state, train)),
                    Cell::from(format!("{}", train.passenger_capacity().passengers())),
                    Cell::from(format_speed_kmh(train)),
                    Cell::from(format_money_per_kilometre(
                        train.fuel_cost_per_kilometre().cents_per_kilometre(),
                    )),
                ])
            } else {
                Row::new([
                    Cell::from(train.name().to_owned()),
                    Cell::from(format_money(train.purchase_price())),
                    Cell::from(funds_after_purchase_display(state, train)),
                    Cell::from(format!("{}", train.passenger_capacity().passengers())),
                ])
            }
        })
        .collect::<Vec<_>>();

    let (headers, widths) = if wide {
        (
            vec!["Model", "Price", "Cash after", "Seats", "Speed", "Fuel/km"],
            vec![
                Constraint::Percentage(23),
                Constraint::Percentage(17),
                Constraint::Percentage(20),
                Constraint::Percentage(10),
                Constraint::Percentage(14),
                Constraint::Percentage(16),
            ],
        )
    } else {
        (
            vec!["Model", "Price", "Cash after", "Seats"],
            vec![
                Constraint::Percentage(30),
                Constraint::Percentage(23),
                Constraint::Percentage(29),
                Constraint::Percentage(18),
            ],
        )
    };

    let market_title = format!("Train Market · {} models", catalogue.len());
    let table = Table::new(rows, widths)
        .header(
            Row::new(headers)
                .style(theme::table_header())
                .bottom_margin(1),
        )
        .block(panel_block(&market_title, true))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut selection.table_state);

    let selected_train = selection
        .selected_catalogue_index(state)
        .and_then(|index| catalogue.get(index));
    render_catalogue_inspector(frame, inspector_area, state, selected_train, wide);
}

fn render_catalogue_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    train: Option<&TrainModel>,
    wide: bool,
) {
    let Some(train) = train else {
        return;
    };

    let (status, status_style) = purchase_status(state, train);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(train.name().to_owned(), theme::focused_title()),
            Span::styled(format!("  {status}"), status_style),
        ]),
        Line::from(""),
        labelled_value("Price", &format_money(train.purchase_price())),
        labelled_value("Company Funds", &format_money(state.player_company.funds)),
        labelled_value(
            "Cash after purchase",
            &funds_after_purchase_display(state, train),
        ),
        Line::from(""),
        Line::styled("VEHICLE IDENTITY", theme::secondary()),
        labelled_value(
            "EVN type",
            &format!("{:02} · {}", train.evn_type_code(), train.evn_type_label()),
        ),
        labelled_value(
            "Registration",
            &format!(
                "{:02} · {}",
                state.region.railway_registration.numeric_code,
                state.region.railway_registration.mark
            ),
        ),
        labelled_value("Official EVN", "assigned on purchase"),
        Line::from(""),
        Line::styled("OPERATING PROFILE", theme::secondary()),
        labelled_value(
            "Capacity",
            &format!("{} passengers", train.passenger_capacity().passengers()),
        ),
        labelled_value("Maximum speed", &format_speed_kmh(train)),
        labelled_value(
            "Fuel",
            &format!(
                "{}/km",
                format_money_per_kilometre(
                    train.fuel_cost_per_kilometre().cents_per_kilometre()
                )
            ),
        ),
    ];

    if wide {
        lines.push(Line::from(""));
        lines.push(Line::styled("RESERVE CHECK", theme::secondary()));
        if let Some(sample) = sample_trip(state, train) {
            lines.push(Line::from(format!(
                "{} · {}",
                sample.route, sample.distance
            )));
            lines.push(labelled_value(
                "Sample departure",
                &format_money(sample.departure_cost),
            ));
            lines.push(labelled_value(
                "Reserve afterwards",
                &reserve_after_sample_display(state, train, &sample),
            ));
        } else {
            lines.push(Line::styled(
                "No sample Rail Line is available.",
                theme::secondary(),
            ));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::styled(
        "Enter · choose delivery Rail Station",
        theme::hint(),
    ));

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Purchase decision", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn labelled_value(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn purchase_status(
    state: &GameState,
    train: &TrainModel,
) -> (&'static str, ratatui::style::Style) {
    if state.player_company.funds < train.purchase_price() {
        ("UNAFFORDABLE", theme::error())
    } else if low_reserve(state, train) {
        ("LOW RESERVE", theme::warning())
    } else {
        ("AFFORDABLE", theme::success())
    }
}

fn funds_after_purchase_display(
    state: &GameState,
    train: &TrainModel,
) -> String {
    if state.player_company.funds < train.purchase_price() {
        return "insufficient".into();
    }
    funds_after_purchase(state, train)
}

fn reserve_after_sample_display(
    state: &GameState,
    train: &TrainModel,
    sample: &SampleTrip,
) -> String {
    let Ok(after_purchase) = state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    else {
        return "unavailable".into();
    };
    if after_purchase < Money::ZERO {
        return "insufficient".into();
    }
    match after_purchase.checked_sub(sample.departure_cost) {
        Ok(remaining) if remaining >= Money::ZERO => format_money(remaining),
        Ok(_) => "insufficient".into(),
        Err(_) => "unavailable".into(),
    }
}

struct DeliveryChooserContext<'a> {
    state: &'a GameState,
    catalogue_index: usize,
    rejection: Option<&'a str>,
}

fn render_delivery_chooser(
    frame: &mut Frame,
    area: Rect,
    chooser: DeliveryChooserContext<'_>,
    selected_delivery_station_id: &mut Option<RailStationId>,
    list_state: &mut ListState,
    page_size: &mut usize,
) {
    let state = chooser.state;
    let stations = delivery_station_ids(state);
    if stations.is_empty() {
        render_delivery_unavailable(
            frame,
            area,
            "No connected Rail Station is available for delivery.",
        );
        return;
    }
    let _ = synchronize_delivery_selection(selected_delivery_station_id, list_state, &stations);

    let footer_rows = u16::from(chooser.rejection.is_some()).saturating_add(1);
    let [step_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            "1 Train → 2 Delivery Rail Station → 3 Review",
            theme::focused_title(),
        ))
        .style(theme::panel()),
        step_area,
    );

    let wide = body_area.width >= 96 && body_area.height >= 10;
    let (list_area, inspector_area) = if wide {
        let [list_area, inspector_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(34)])
                .spacing(1)
                .areas(body_area);
        (list_area, Some(inspector_area))
    } else {
        (body_area, None)
    };
    let visible_items = usize::from(list_area.height.saturating_sub(2)).max(1);
    *page_size = visible_items;
    keep_delivery_selection_visible(list_state, visible_items);

    let items = stations
        .iter()
        .map(|station_id| ListItem::new(station_label(state, *station_id).to_owned()))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(panel_block("Delivery Rail Stations", true))
        .style(theme::panel())
        .highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, list_area, list_state);

    if let Some(inspector_area) = inspector_area {
        render_delivery_inspector(
            frame,
            inspector_area,
            state,
            chooser.catalogue_index,
            *selected_delivery_station_id,
        );
    }

    let controls = if area.width <= 80 {
        "↑↓/J K · station  Enter · review  Left · model  Esc · cancel"
    } else {
        "↑↓ / J K · select station   PageUp / PageDown · scroll   Enter · review   Left / Backspace · model   Esc · cancel"
    };
    let mut footer = vec![Line::styled(controls, theme::hint())];
    if let Some(rejection) = chooser.rejection {
        footer.insert(
            0,
            Line::styled(format!("Delivery unavailable: {rejection}"), theme::error()),
        );
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn render_delivery_unavailable(frame: &mut Frame, area: Rect, reason: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "1 Train → 2 Delivery Rail Station → 3 Review",
                theme::focused_title(),
            ),
            Line::styled(reason, theme::error()),
            Line::styled("Left / Backspace · model   Esc · cancel", theme::hint()),
        ])
        .block(panel_block("Train Market · delivery unavailable", true))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_delivery_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    catalogue_index: usize,
    selected_delivery_station_id: Option<RailStationId>,
) {
    let selected_station = selected_delivery_station_id
        .map(|station_id| station_label(state, station_id))
        .unwrap_or("No Rail Station selected");
    let selected_train = train_catalogue()
        .models()
        .get(catalogue_index)
        .map_or(
            "Selected catalogue Train unavailable",
            TrainModel::name,
        );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Delivering", theme::secondary()),
            Line::styled(selected_train, theme::title()),
            Line::from(""),
            Line::styled("Rail Station", theme::secondary()),
            Line::styled(selected_station, theme::focused_title()),
            Line::from(""),
            Line::styled("Delivery has no fee.", theme::secondary()),
            Line::styled("Enter · continue to review", theme::hint()),
        ])
        .block(panel_block("Selected delivery", false))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(theme::THIN_BORDERS)
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

fn render_selected(state: &GameState, selected_catalogue_index: usize) -> String {
    let mut output = String::from("Market\n");
    for (index, train) in train_catalogue().models().iter().enumerate() {
        render_catalogue_train(&mut output, state, index, train);
    }
    if let Some(train) = train_catalogue()
        .models()
        .get(selected_catalogue_index)
    {
        writeln!(output, "Selected Train: {}", train.name())
            .expect("writing to a String cannot fail");
        render_purchase_implications(&mut output, state, train);
    }
    output
}

fn render_catalogue_train(
    output: &mut String,
    state: &GameState,
    index: usize,
    train: &TrainModel,
) {
    writeln!(output, "{}: {}", index + 1, train.name()).expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Capacity: {} passengers | Speed: {} | Fuel Cost: {}/km",
        train.passenger_capacity().passengers(),
        format_speed_kmh(train),
        format_money_per_kilometre(train.fuel_cost_per_kilometre().cents_per_kilometre()),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Purchase price: {}",
        format_money(train.purchase_price())
    )
    .expect("writing to a String cannot fail");
    if let Some(sample) = sample_trip(state, train) {
        writeln!(
            output,
            "  Sample trip ({}, {}): Infrastructure Access Fee {} + Fuel Cost {} = {} departure cost",
            sample.route,
            sample.distance,
            format_money(sample.access_fee),
            format_money(sample.fuel_cost),
            format_money(sample.departure_cost),
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_purchase_implications(
    output: &mut String,
    state: &GameState,
    train: &TrainModel,
) {
    match state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    {
        Ok(remaining) if remaining >= Money::ZERO => writeln!(
            output,
            "Company Funds after purchase: {}",
            format_money(remaining)
        )
        .expect("writing to a String cannot fail"),
        Ok(_) => writeln!(
            output,
            "Company Funds after purchase: insufficient Company Funds (currently {})",
            format_money(state.player_company.funds)
        )
        .expect("writing to a String cannot fail"),
        Err(_) => writeln!(output, "Company Funds after purchase: unavailable")
            .expect("writing to a String cannot fail"),
    }
}

struct SampleTrip {
    route: String,
    distance: String,
    access_fee: Money,
    fuel_cost: Money,
    departure_cost: Money,
}

fn sample_trip(state: &GameState, train: &TrainModel) -> Option<SampleTrip> {
    let line = state
        .region
        .rail_authority
        .rail_network
        .rail_lines
        .iter()
        .min_by_key(|line| line.distance.metres())?;
    let access_fee = state
        .rules
        .balance
        .access_fee_per_train_kilometre()
        .checked_charge(line.distance)
        .ok()?;
    let fuel_cost = train
        .fuel_cost_per_kilometre()
        .checked_charge(line.distance)
        .ok()?;
    let departure_cost = access_fee.checked_add(fuel_cost).ok()?;
    Some(SampleTrip {
        route: format!(
            "{} -> {}",
            station_label(state, line.first_station_id),
            station_label(state, line.second_station_id)
        ),
        distance: format_distance(line),
        access_fee,
        fuel_cost,
        departure_cost,
    })
}

fn low_reserve(state: &GameState, train: &TrainModel) -> bool {
    let Ok(remaining) = state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    else {
        return true;
    };
    remaining < Money::ZERO
        || sample_trip(state, train).is_some_and(|sample| remaining < sample.departure_cost)
}

fn delivery_station_ids(state: &GameState) -> Vec<RailStationId> {
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect()
}

fn synchronize_delivery_selection(
    selected_delivery_station_id: &mut Option<RailStationId>,
    list_state: &mut ListState,
    stations: &[RailStationId],
) -> bool {
    let previous = *selected_delivery_station_id;
    let fallback = list_state.selected().unwrap_or(0).min(stations.len() - 1);
    let selected = previous
        .and_then(|station_id| {
            stations
                .iter()
                .position(|candidate| *candidate == station_id)
        })
        .or_else(|| (!stations.is_empty()).then_some(fallback));
    if let Some(index) = selected {
        *selected_delivery_station_id = Some(stations[index]);
        list_state.select(Some(index));
    } else {
        *selected_delivery_station_id = None;
        list_state.select(None);
        *list_state.offset_mut() = 0;
    }
    previous.is_some() && previous != *selected_delivery_station_id
}

fn move_delivery_selection(
    selected_delivery_station_id: &mut Option<RailStationId>,
    list_state: &mut ListState,
    stations: &[RailStationId],
    page_size: usize,
    key: KeyCode,
) {
    let current = selected_delivery_station_id.and_then(|station_id| {
        stations
            .iter()
            .position(|candidate| *candidate == station_id)
    });
    let last = stations.len().saturating_sub(1);
    let next = match key {
        KeyCode::Up | KeyCode::Char('k' | 'K') => {
            current.map_or(0, |index| index.saturating_sub(1))
        }
        KeyCode::Down | KeyCode::Char('j' | 'J') => {
            current.map_or(0, |index| index.saturating_add(1).min(last))
        }
        KeyCode::PageUp => current.map_or(0, |index| index.saturating_sub(page_size.max(1))),
        KeyCode::PageDown => {
            current.map_or(0, |index| index.saturating_add(page_size.max(1)).min(last))
        }
        _ => return,
    };
    *selected_delivery_station_id = stations.get(next).copied();
    list_state.select(Some(next));
}

fn keep_delivery_selection_visible(list_state: &mut ListState, visible_items: usize) {
    let Some(selected) = list_state.selected() else {
        return;
    };
    let visible_items = visible_items.max(1);
    let offset = list_state.offset();
    if selected < offset {
        *list_state.offset_mut() = selected;
    } else if selected >= offset.saturating_add(visible_items) {
        *list_state.offset_mut() = selected.saturating_add(1).saturating_sub(visible_items);
    }
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

fn format_distance(line: &RailLine) -> String {
    crate::ui::format::distance(line.distance.metres())
}

fn format_money_per_kilometre(cents: u64) -> String {
    format_money(Money::from_cents(i64::try_from(cents).unwrap_or(i64::MAX)))
}

fn format_speed_kmh(train: &TrainModel) -> String {
    crate::ui::format::speed_kmh(train.speed().metres_per_second())
}

fn format_money(money: Money) -> String {
    crate::ui::format::money(money)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{MarketFlow, MarketFlowAction, render};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn catalogue_shows_both_diesel_trains_with_price_stats_and_sample_cost() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let rendered = render(&state);

        for train in train_catalogue().models() {
            assert!(rendered.contains(train.name()));
            assert!(rendered.contains(&format!(
                "Capacity: {} passengers",
                train.passenger_capacity().passengers()
            )));
        }
        assert!(rendered.contains("Speed:"));
        assert!(rendered.contains("Fuel Cost:"));
        assert!(rendered.contains("Purchase price:"));
        assert!(rendered.contains("Sample trip"));
        assert!(rendered.contains("Infrastructure Access Fee"));
    }

    #[test]
    fn flow_selects_a_connected_delivery_station_and_requires_confirmation() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut flow = MarketFlow::start(&state, 1).unwrap();

        let delivery_selection = flow.render(&state);
        for station in &state.region.rail_authority.rail_network.rail_stations {
            let settlement = state
                .region
                .settlements
                .iter()
                .find(|settlement| settlement.id == station.settlement_id)
                .unwrap();
            assert!(delivery_selection.contains(&settlement.name));
        }

        flow.handle_key(key(KeyCode::Down), &state);
        flow.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            MarketFlowAction::Confirm {
                catalogue_index: 1,
                delivery_station_id: RailStationId::new(2),
            }
        );
    }

    #[test]
    fn confirmation_warns_when_purchase_leaves_no_sample_trip_reserve() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::from_cents(500_000);
        let mut flow = MarketFlow::start(&state, 1).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);

        let rendered = flow.render(&state);
        assert!(rendered.contains("Company Funds after purchase: $0.00"));
        assert!(rendered.contains("LOW RESERVE:"));
        assert!(rendered.contains("Enter confirms purchase"));
    }
}
