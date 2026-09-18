//! Keyboard state and text presentation for buying catalogue Trains.
//!
//! A proposed purchase remains presentation-only until the Player Company
//! explicitly confirms it. The application boundary then revalidates the
//! catalogue entry, selected Rail Station, and Company Funds before saving.

use std::fmt::Write;

mod dashboard;
mod inspector;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{
        Cell, HighlightSpacing, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
};

use crate::{
    catalog::{TrainModel, train_catalogue},
    model::{GameState, Money, RailLine, RailStationId, TrainStatus},
    ui::{
        components::{EmptyState, panel_block},
        modal, theme,
    },
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

/// Shell-facing outcome from the Market workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketWorkspaceAction {
    /// No application action is required.
    Continue,
    /// Clear stale Shell feedback after a successful Market-only transition.
    ClearNotice,
    /// Surface presentation feedback without crossing the application boundary.
    Notice(String),
    /// Revalidate and persist the proposed Train purchase.
    Purchase {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
}

/// One contextual footer action owned by the Market workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl MarketShortcut {
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

/// Owns all presentation state and keyboard interaction for the Train Market.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarketWorkspace {
    flow: Option<MarketFlow>,
    selection: CatalogueSelection,
}

impl MarketWorkspace {
    /// Returns whether an uncommitted purchase flow currently owns input.
    pub fn has_flow(&self) -> bool {
        self.flow.is_some()
    }

    /// Returns whether delivery Rail Station selection currently owns the content area.
    fn is_selecting_delivery(&self) -> bool {
        self.flow
            .as_ref()
            .is_some_and(MarketFlow::is_selecting_delivery)
    }

    /// Returns whether this workspace currently owns the focused modal layer.
    ///
    /// The complete purchase workflow stays above the Market dashboard so
    /// delivery selection and final review feel like one coherent transaction.
    pub fn has_modal(&self) -> bool {
        self.flow.is_some()
    }

    /// Returns the contextual footer actions for the current Market step.
    pub fn shortcuts(
        &mut self,
        state: &GameState,
        compact: bool,
        wide: bool,
    ) -> Vec<MarketShortcut> {
        if self.has_flow() {
            if self.is_selecting_delivery() {
                let mut items = vec![MarketShortcut::enabled(
                    if compact { "↑↓" } else { "↑↓/JK" },
                    "Station",
                )];
                if wide {
                    items.push(MarketShortcut::enabled("PgUp/PgDn", "Page"));
                }
                items.extend([
                    MarketShortcut::enabled("Enter", "Review"),
                    MarketShortcut::enabled("←", "Model"),
                    MarketShortcut::enabled("Esc", "Cancel"),
                ]);
                return items;
            }

            return vec![
                MarketShortcut::enabled("Enter", "Purchase"),
                MarketShortcut::enabled("←", "Back"),
                MarketShortcut::enabled("Esc", "Cancel"),
            ];
        }

        let mut items = vec![MarketShortcut::enabled(
            if compact { "↑↓" } else { "↑↓/JK" },
            "Model",
        )];
        if wide {
            items.push(MarketShortcut::enabled("PgUp/PgDn", "Page"));
        }
        items.push(if self.purchase_available(state) {
            MarketShortcut::enabled("Enter", "Buy")
        } else {
            MarketShortcut::disabled("Enter", "Buy")
        });
        items
    }

    /// Returns focused help text when a purchase flow owns input.
    pub fn help_lines(&self) -> Option<Vec<String>> {
        if !self.has_flow() {
            return None;
        }

        let mut lines = vec!["Current · Train Purchase".into()];
        if self.is_selecting_delivery() {
            lines.extend([
                "↑↓ / jk Select the delivery Rail Station".into(),
                "Enter Review purchase".into(),
                "← / Backspace Previous step   Esc Cancel".into(),
            ]);
        } else {
            lines.extend([
                "Enter Confirm purchase".into(),
                "← / Backspace Previous step   Esc Cancel".into(),
            ]);
        }
        Some(lines)
    }

    /// Routes one Market-owned keyboard event. Global view navigation remains a Shell concern.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> MarketWorkspaceAction {
        if self.flow.is_some() {
            let action = self
                .flow
                .as_mut()
                .expect("checked Market flow presence")
                .handle_key(key, state);
            return match action {
                MarketFlowAction::Continue | MarketFlowAction::ReturnToDelivery => {
                    MarketWorkspaceAction::Continue
                }
                MarketFlowAction::Cancel => {
                    self.flow = None;
                    MarketWorkspaceAction::Notice(
                        "Train purchase cancelled; no changes were made.".into(),
                    )
                }
                MarketFlowAction::ReturnToCatalogue => {
                    self.flow = None;
                    MarketWorkspaceAction::ClearNotice
                }
                MarketFlowAction::Confirm {
                    catalogue_index,
                    delivery_station_id,
                } => MarketWorkspaceAction::Purchase {
                    catalogue_index,
                    delivery_station_id,
                },
            };
        }

        match key.code {
            KeyCode::Enter => match self.selection.selected_catalogue_index(state) {
                Some(catalogue_index) => match MarketFlow::start(state, catalogue_index) {
                    Ok(flow) => {
                        self.flow = Some(flow);
                        MarketWorkspaceAction::ClearNotice
                    }
                    Err(message) => MarketWorkspaceAction::Notice(message.into()),
                },
                None => MarketWorkspaceAction::Notice(
                    "No diesel Train is available in the catalogue.".into(),
                ),
            },
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                self.selection.handle_key(key.code, state);
                MarketWorkspaceAction::Continue
            }
            _ => MarketWorkspaceAction::Continue,
        }
    }

    /// Returns whether the currently focused catalogue Train can start a purchase.
    pub fn purchase_available(&mut self, state: &GameState) -> bool {
        self.selection
            .selected_catalogue_index(state)
            .is_some_and(|catalogue_index| purchase_action_available(state, catalogue_index))
    }

    /// Records an application-boundary rejection while preserving purchase context.
    pub fn reject(&mut self, error: impl Into<String>) -> Option<String> {
        let error = error.into();
        if let Some(flow) = &mut self.flow {
            flow.reject(error);
            None
        } else {
            Some(error)
        }
    }

    /// Closes the purchase flow after persistence succeeds.
    pub fn confirm_saved(&mut self) {
        self.flow = None;
    }

    /// Clears transient Market state, for example when starting a fresh game.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Renders the active Market content layer.
    ///
    /// Purchase steps are rendered as a modal layer by the Shell. Keeping the
    /// catalogue visible underneath preserves context throughout the workflow.
    pub fn render_dashboard(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_dashboard(frame, area, state, &mut self.selection);
    }

    /// Renders the compact text fallback used by the Shell.
    pub fn render_text(&self, state: &GameState) -> String {
        match &self.flow {
            Some(flow) => flow.render(state),
            None => render(state),
        }
    }

    /// Renders the focused purchase-confirmation layer when one is active.
    pub fn render_modal(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(flow) = &mut self.flow {
            flow.render_panel(frame, area, state);
        }
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
        if let Some(error) = purchase_start_error(state, catalogue_index) {
            return Err(error);
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
                if train_catalogue().models().get(*catalogue_index).is_none() {
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
                modal::workflow_rect(area),
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
                modal::workflow_rect(area),
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

/// Returns whether the selected catalogue model can enter the purchase flow.
///
/// The footer uses this to keep the Buy action visible while muting it when the
/// current model cannot be purchased. `MarketFlow::start` uses the same rules so
/// keyboard behaviour never disagrees with the action bar.
pub fn purchase_action_available(state: &GameState, catalogue_index: usize) -> bool {
    purchase_start_error(state, catalogue_index).is_none()
}

fn purchase_start_error(state: &GameState, catalogue_index: usize) -> Option<&'static str> {
    if train_catalogue().models().is_empty() {
        return Some("No diesel Train is available in the catalogue.");
    }
    let Some(train) = train_catalogue().models().get(catalogue_index) else {
        return Some("The selected catalogue Train is no longer available.");
    };
    if state.player_company.funds < train.purchase_price() {
        return Some("Insufficient Company Funds for the selected Train.");
    }
    if delivery_station_ids(state).is_empty() {
        return Some("No connected Rail Station is available for delivery.");
    }
    None
}

fn render_purchase_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
    rejection: Option<&str>,
) {
    let footer = modal::shortcut_line(&[
        modal::ModalShortcut::enabled("Enter", modal::ModalAction::Confirm),
        modal::ModalShortcut::enabled("←", modal::ModalAction::Delivery),
        modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
    ]);
    let modal_areas = modal::render_shell(frame, area, "Purchase Train", footer);

    let Some(train) = train_catalogue().models().get(catalogue_index) else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Purchase unavailable", theme::error()),
                Line::from(""),
                Line::from(
                    "The selected catalogue Train is no longer available. Return to delivery and choose a current model.",
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            modal_areas.body,
        );
        return;
    };

    let rejection_rows = u16::from(rejection.is_some());
    let [summary_area, review_area, rejection_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(rejection_rows),
    ])
    .areas(modal_areas.body);

    render_purchase_workflow_summary(
        frame,
        summary_area,
        state,
        train,
        "TRAIN ✓   DELIVERY ✓   REVIEW ●",
        false,
    );

    if review_area.width >= 78 && review_area.height >= 9 {
        let [order_area, financial_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(1)
                .areas(review_area);

        let keeper_mark = format!(
            "{}-{}",
            state.region.railway_registration.mark,
            state.player_company.vehicle_keeper_mark.as_str(),
        );
        let order_lines = vec![
            review_value("Model", train.name()),
            review_value("Delivery", &station_label(state, delivery_station_id)),
            review_value("Delivery fee", "$0.00"),
            Line::from(""),
            Line::styled("REGISTRATION", theme::secondary()),
            review_value("EVN type", &format!("{:02}", train.evn_type_code())),
            review_value("EVN series", &format!("{:04}", train.evn_series_code())),
            review_value(
                "Registration",
                &format!(
                    "{:02} · {}",
                    state.region.railway_registration.numeric_code,
                    state.region.railway_registration.mark
                ),
            ),
            review_value("Keeper mark", &keeper_mark),
            review_value("Official EVN", "Assigned on purchase"),
        ];
        frame.render_widget(
            Paragraph::new(order_lines)
                .block(panel_block("Order", false))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            order_area,
        );

        let mut financial_lines = vec![
            review_value("Purchase price", &format_money(train.purchase_price())),
            review_value("Cash before", &format_money(state.player_company.funds)),
            review_value("Cash after", &funds_after_purchase(state, train)),
        ];
        if let Some(sample) = sample_trip(state, train) {
            financial_lines.extend([
                Line::from(""),
                Line::styled("OPERATING RESERVE", theme::secondary()),
                review_value("Benchmark", &sample.route),
                review_value("Distance", &sample.distance),
                review_value("Trip cost", &format_money(sample.departure_cost)),
                review_value(
                    "Cash after trip",
                    &reserve_after_sample_display(state, train, &sample),
                ),
            ]);
        }
        financial_lines.push(Line::from(""));
        financial_lines.push(if low_reserve(state, train) {
            Line::styled(
                "LOW RESERVE · operating reserve not covered.",
                theme::warning(),
            )
        } else {
            Line::styled("READY TO PURCHASE · operating reserve covered.", theme::success())
        });
        frame.render_widget(
            Paragraph::new(financial_lines)
                .block(panel_block("Financial", true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            financial_area,
        );
    } else {
        let mut lines = compact_purchase_confirmation_lines(state, train, delivery_station_id);
        lines.push(review_value(
            "EVN",
            &format!(
                "{:02} {:02} {:04} · assigned on purchase",
                train.evn_type_code(),
                state.region.railway_registration.numeric_code,
                train.evn_series_code()
            ),
        ));
        if low_reserve(state, train) {
            lines.push(Line::styled(
                "LOW RESERVE · operating reserve not covered.",
                theme::warning(),
            ));
        } else {
            lines.push(Line::styled("READY TO PURCHASE", theme::success()));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel_block("Review", true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            review_area,
        );
    }

    if let Some(rejection) = rejection {
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!("Purchase rejected: {rejection}"),
                theme::error(),
            ))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            rejection_area,
        );
    }
}

fn render_purchase_workflow_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    train: &TrainModel,
    step: &'static str,
    show_financial_preview: bool,
) {
    let mut lines = vec![
        Line::styled(step, theme::focused_title()),
        Line::from(vec![
            Span::styled(train.name().to_owned(), theme::focused_title()),
            Span::styled(
                format!(
                    "   {} seats · {} · {}",
                    train.passenger_capacity().passengers(),
                    format_speed_kmh(train),
                    train.propulsion_label()
                ),
                theme::secondary(),
            ),
        ]),
    ];
    if show_financial_preview {
        lines.push(Line::from(vec![
            Span::styled("Purchase ", theme::secondary()),
            Span::styled(format_money(train.purchase_price()), theme::primary_value()),
            Span::styled("   Cash after ", theme::secondary()),
            Span::styled(funds_after_purchase(state, train), theme::primary_value()),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn compact_purchase_confirmation_lines(
    state: &GameState,
    train: &TrainModel,
    delivery_station_id: RailStationId,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        review_value("Delivery", &station_label(state, delivery_station_id)),
        review_value("Price", &format_money(train.purchase_price())),
        review_value("Cash after", &funds_after_purchase(state, train)),
        review_value(
            "Train",
            &format!(
                "{} seats · {} · {}",
                train.passenger_capacity().passengers(),
                format_speed_kmh(train),
                train.propulsion_label()
            ),
        ),
    ];
    if let Some(sample) = sample_trip(state, train) {
        lines.push(review_value(
            "Operating reserve",
            &format!("{} · {}", sample.route, format_money(sample.departure_cost)),
        ));
    }
    lines
}

fn review_value(label: &str, value: &str) -> Line<'static> {
    const LABEL_WIDTH: usize = 17;
    Line::from(vec![
        Span::styled(
            format!("{label:<width$}", width = LABEL_WIDTH),
            theme::secondary(),
        ),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
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
            "LOW RESERVE: funds after purchase cannot cover this benchmark trip cost."
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
/// Market is a single procurement workspace: the overview answers “what does
/// this choice mean?”, the catalogue answers “which model?”, and the inspector
/// owns the deeper specification. Purchase actions stay in the global footer.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut CatalogueSelection,
) {
    selection.synchronize(state);
    let catalogue = train_catalogue().models();
    if catalogue.is_empty() {
        let shell = panel_block("Market", true);
        let inner = shell.inner(area);
        frame.render_widget(shell, area);
        EmptyState::temporary(
            "Catalogue unavailable",
            "No rolling-stock models are available in this build.",
        )
        .motif("╾━╼")
        .hint("There is nothing to purchase until catalogue data becomes available.")
        .render(frame, inner);
        return;
    }

    let selected_train = selection
        .selected_catalogue_index(state)
        .and_then(|index| catalogue.get(index));

    let shell = panel_block("Market", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let show_metrics = shell_inner.width >= 88 && shell_inner.height >= 18;
    let (overview_area, metrics_area, market_area) = if show_metrics {
        let [overview_area, metrics_area, market_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .areas(shell_inner);
        (overview_area, Some(metrics_area), market_area)
    } else {
        let [overview_area, market_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)])
                .spacing(1)
                .areas(shell_inner);
        (overview_area, None, market_area)
    };
    dashboard::render_overview(frame, overview_area, state, selected_train);
    if let Some(metrics_area) = metrics_area {
        dashboard::render_metrics(frame, metrics_area, state, selected_train);
    }

    let wide = market_area.width >= 96 && market_area.height >= 12;
    let (catalogue_area, inspector_area) = if wide {
        let [catalogue_area, inspector_area] =
            Layout::horizontal([Constraint::Min(42), Constraint::Length(48)]).areas(market_area);
        (horizontal_inset(catalogue_area, 1), inspector_area)
    } else {
        let catalogue_height = market_area.height.min(5);
        let [catalogue_area, inspector_area] =
            Layout::vertical([Constraint::Length(catalogue_height), Constraint::Min(8)])
                .areas(market_area);
        (horizontal_inset(catalogue_area, 1), inspector_area)
    };

    let [catalogue_heading_area, catalogue_table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(catalogue_area);
    frame.render_widget(
        Paragraph::new(Line::styled("CATALOGUE", theme::secondary())).style(theme::panel()),
        catalogue_heading_area,
    );

    selection.set_page_size(usize::from(catalogue_table_area.height.saturating_sub(2)).max(1));
    selection.synchronize(state);

    let owned_count = |train: &TrainModel| catalogue_ownership(state, train).owned;

    // Keep the catalogue focused on comparisons. Normal affordability is not
    // repeated row-by-row; exceptional purchase states are communicated by
    // price styling and the selected-model overview/inspector.
    let (rows, widths, headers) = if catalogue_table_area.width >= 82 {
        (
            catalogue
                .iter()
                .map(|train| {
                    Row::new(vec![
                        Cell::from(train.name().to_owned()),
                        right_cell(train.passenger_capacity().passengers().to_string()),
                        right_cell(format_speed_kmh(train)),
                        Cell::from(train.propulsion_label()),
                        right_cell(format!(
                            "{}/km",
                            format_money_per_kilometre(
                                train.fuel_cost_per_kilometre().cents_per_kilometre()
                            )
                        )),
                        right_cell(owned_count(train).to_string()),
                        purchase_price_cell(state, train),
                    ])
                })
                .collect::<Vec<_>>(),
            vec![
                Constraint::Min(18),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(13),
            ],
            vec![
                "Model",
                "Seats",
                "Top speed",
                "Propulsion",
                "Fuel/km",
                "Owned",
                "Price",
            ],
        )
    } else if catalogue_table_area.width >= 66 {
        (
            catalogue
                .iter()
                .map(|train| {
                    Row::new(vec![
                        Cell::from(train.name().to_owned()),
                        right_cell(train.passenger_capacity().passengers().to_string()),
                        right_cell(format_speed_kmh(train)),
                        right_cell(format!(
                            "{}/km",
                            format_money_per_kilometre(
                                train.fuel_cost_per_kilometre().cents_per_kilometre()
                            )
                        )),
                        right_cell(owned_count(train).to_string()),
                        purchase_price_cell(state, train),
                    ])
                })
                .collect::<Vec<_>>(),
            vec![
                Constraint::Min(16),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(13),
            ],
            vec!["Model", "Seats", "Top speed", "Fuel/km", "Owned", "Price"],
        )
    } else if catalogue_table_area.width >= 58 {
        (
            catalogue
                .iter()
                .map(|train| {
                    Row::new(vec![
                        Cell::from(train.name().to_owned()),
                        right_cell(train.passenger_capacity().passengers().to_string()),
                        right_cell(format_speed_kmh(train)),
                        purchase_price_cell(state, train),
                    ])
                })
                .collect::<Vec<_>>(),
            vec![
                Constraint::Min(16),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(13),
            ],
            vec!["Model", "Seats", "Top speed", "Price"],
        )
    } else {
        (
            catalogue
                .iter()
                .map(|train| {
                    Row::new(vec![
                        Cell::from(train.name().to_owned()),
                        purchase_price_cell(state, train),
                    ])
                })
                .collect::<Vec<_>>(),
            vec![Constraint::Min(18), Constraint::Length(13)],
            vec!["Model", "Price"],
        )
    };

    let table = Table::new(rows, widths)
        .header(
            catalogue_header_row(headers)
                .style(theme::table_header())
                .bottom_margin(1),
        )
        .style(theme::panel())
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, catalogue_table_area, &mut selection.table_state);

    inspector::render_catalogue_inspector(frame, inspector_area, state, selected_train, wide, true);
}

fn catalogue_header_row(headers: Vec<&str>) -> Row<'static> {
    Row::new(headers.into_iter().map(|header| {
        if matches!(
            header,
            "Seats" | "Top speed" | "Fuel/km" | "Owned" | "Price"
        ) {
            Cell::from(Text::from(header.to_owned()).right_aligned())
        } else {
            Cell::from(header.to_owned())
        }
    }))
}

fn right_cell(value: impl Into<String>) -> Cell<'static> {
    Cell::from(Text::from(value.into()).right_aligned())
}

fn purchase_price_cell(state: &GameState, train: &TrainModel) -> Cell<'static> {
    let cell = right_cell(format_money(train.purchase_price()));
    if state.player_company.funds < train.purchase_price() {
        cell.style(theme::error())
    } else if delivery_station_ids(state).is_empty() || low_reserve(state, train) {
        cell.style(theme::warning())
    } else {
        cell
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CatalogueOwnership {
    owned: usize,
    ready: usize,
    travelling: usize,
}

fn catalogue_ownership(state: &GameState, train: &TrainModel) -> CatalogueOwnership {
    let mut ownership = CatalogueOwnership::default();
    for owned in state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|owned| &owned.model_id == train.id())
    {
        ownership.owned += 1;
        match &owned.status {
            TrainStatus::Ready { .. } => ownership.ready += 1,
            TrainStatus::Travelling { .. } => ownership.travelling += 1,
        }
    }
    ownership
}

fn horizontal_inset(area: Rect, amount: u16) -> Rect {
    let total = amount.saturating_mul(2);
    Rect {
        x: area.x.saturating_add(amount.min(area.width)),
        y: area.y,
        width: area.width.saturating_sub(total),
        height: area.height,
    }
}

fn funds_after_purchase_display(state: &GameState, train: &TrainModel) -> String {
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
    let mut footer_shortcuts = vec![
        modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
        modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Station),
        modal::ModalShortcut::enabled("←", modal::ModalAction::Train),
        modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
    ];
    if area.width >= 82 {
        footer_shortcuts.push(modal::ModalShortcut::enabled(
            "PgUp/PgDn",
            modal::ModalAction::Page,
        ));
    }
    let footer = modal::shortcut_line(&footer_shortcuts);
    let modal_areas = modal::render_shell(frame, area, "Purchase Train", footer);
    let state = chooser.state;

    let Some(train) = train_catalogue().models().get(chooser.catalogue_index) else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Purchase unavailable", theme::error()),
                Line::from(""),
                Line::from(
                    "The selected catalogue Train is no longer available. Return to the catalogue and choose a current model.",
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            modal_areas.body,
        );
        return;
    };

    let stations = delivery_station_ids(state);
    if stations.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("TRAIN ✓   DELIVERY ●   REVIEW ○", theme::focused_title()),
                Line::from(""),
                Line::styled(
                    "No connected Rail Station is available for delivery.",
                    theme::error(),
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            modal_areas.body,
        );
        return;
    }
    let _ = synchronize_delivery_selection(selected_delivery_station_id, list_state, &stations);

    let rejection_rows = u16::from(chooser.rejection.is_some());
    let [summary_area, body_area, rejection_area] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(5),
        Constraint::Length(rejection_rows),
    ])
    .areas(modal_areas.body);
    render_purchase_workflow_summary(
        frame,
        summary_area,
        state,
        train,
        "TRAIN ✓   DELIVERY ●   REVIEW ○",
        true,
    );

    let wide = body_area.width >= 78 && body_area.height >= 8;
    let (list_area, order_area) = if wide {
        let [list_area, order_area] =
            Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
                .spacing(1)
                .areas(body_area);
        (list_area, Some(order_area))
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
        .block(panel_block("Delivery Station", true))
        .style(theme::panel())
        .highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, list_area, list_state);

    if let Some(order_area) = order_area {
        let selected_station = (*selected_delivery_station_id)
            .map(|station_id| station_label(state, station_id))
            .unwrap_or("No Rail Station selected");
        let keeper_mark = format!(
            "{}-{}",
            state.region.railway_registration.mark,
            state.player_company.vehicle_keeper_mark.as_str(),
        );
        frame.render_widget(
            Paragraph::new(vec![
                review_value("Model", train.name()),
                review_value("Station", selected_station),
                review_value("Delivery fee", "$0.00"),
                Line::from(""),
                Line::styled("REGISTRATION", theme::secondary()),
                review_value(
                    "EVN basis",
                    &format!(
                        "{:02} {:02} {:04}",
                        train.evn_type_code(),
                        state.region.railway_registration.numeric_code,
                        train.evn_series_code()
                    ),
                ),
                review_value("Keeper mark", &keeper_mark),
                review_value("Official EVN", "Assigned on purchase"),
            ])
            .block(panel_block("Order Preview", false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            order_area,
        );
    }

    if let Some(rejection) = chooser.rejection {
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!("Delivery unavailable: {rejection}"),
                theme::error(),
            ))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            rejection_area,
        );
    }
}

fn render_selected(state: &GameState, selected_catalogue_index: usize) -> String {
    let mut output = String::from("Market\n");
    for (index, train) in train_catalogue().models().iter().enumerate() {
        render_catalogue_train(&mut output, state, index, train);
    }
    if let Some(train) = train_catalogue().models().get(selected_catalogue_index) {
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

fn render_purchase_implications(output: &mut String, state: &GameState, train: &TrainModel) {
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
            "{} → {}",
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
        catalog::train_catalogue,
        model::{RailStationId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{MarketFlow, MarketFlowAction, MarketWorkspace, MarketWorkspaceAction, render};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn catalogue_shows_all_diesel_trains_with_price_stats_and_sample_cost() {
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
    fn workspace_owns_catalogue_selection_and_purchase_flow() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MarketWorkspace::default();

        assert_eq!(
            workspace.handle_key(key(KeyCode::Enter), &state),
            MarketWorkspaceAction::ClearNotice
        );
        assert!(workspace.is_selecting_delivery());
        assert!(workspace.has_modal());

        assert_eq!(
            workspace.handle_key(key(KeyCode::Enter), &state),
            MarketWorkspaceAction::Continue
        );
        assert!(workspace.has_modal());

        assert!(matches!(
            workspace.handle_key(key(KeyCode::Enter), &state),
            MarketWorkspaceAction::Purchase {
                catalogue_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn workspace_owns_contextual_controls_and_help_for_purchase_steps() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MarketWorkspace::default();

        let catalogue = workspace.shortcuts(&state, false, true);
        assert!(catalogue.iter().any(|item| item.action == "Model"));
        assert!(catalogue.iter().any(|item| item.action == "Page"));
        assert!(
            catalogue
                .iter()
                .any(|item| item.action == "Buy" && item.enabled)
        );
        assert!(workspace.help_lines().is_none());

        workspace.handle_key(key(KeyCode::Enter), &state);
        let delivery = workspace.shortcuts(&state, false, true);
        assert!(delivery.iter().any(|item| item.action == "Station"));
        assert!(delivery.iter().any(|item| item.action == "Review"));
        assert!(
            workspace
                .help_lines()
                .is_some_and(|lines| lines.iter().any(|line| line == "Enter Review purchase"))
        );

        workspace.handle_key(key(KeyCode::Enter), &state);
        let confirmation = workspace.shortcuts(&state, false, true);
        assert!(confirmation.iter().any(|item| item.action == "Purchase"));
        assert!(workspace.has_modal());
    }

    #[test]
    fn workspace_cancels_purchase_without_leaking_flow_state() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = MarketWorkspace::default();

        workspace.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            workspace.handle_key(key(KeyCode::Esc), &state),
            MarketWorkspaceAction::Notice("Train purchase cancelled; no changes were made.".into())
        );
        assert!(!workspace.has_flow());
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
        state.player_company.funds = train_catalogue().models()[1].purchase_price();
        let mut flow = MarketFlow::start(&state, 1).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);

        let rendered = flow.render(&state);
        assert!(rendered.contains("Company Funds after purchase: $0.00"));
        assert!(rendered.contains("LOW RESERVE:"));
        assert!(rendered.contains("Enter confirms purchase"));
    }
}
