//! Fleet presentation and the confirmed Train resale flow.
//!
//! A resale proposal is kept outside the simulation until the Player Company
//! explicitly confirms it. The application boundary then advances time,
//! revalidates that the Train is READY, and persists the sale.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, LineGauge, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    catalog::{model_for_train, train_catalogue},
    model::{GameState, Journey, Money, RailStationId, Train, TrainId, TrainStatus, UtcSeconds},
    ui::theme,
};

/// Persistent Fleet browsing state. The selected identity is a Train ID so a
/// live arrival or a resale cannot accidentally move the player's focus to a
/// different Train when the collection changes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FleetSelection {
    selected_train_id: Option<TrainId>,
    table_state: TableState,
    page_size: usize,
}

impl FleetSelection {
    /// Selects a known Fleet Train by stable ID without changing the game.
    /// Map Journey inspection uses this to preserve the arrived Train as the
    /// player's Fleet selection after reconciliation removes its Journey.
    pub fn select_train_id(&mut self, state: &GameState, train_id: TrainId) {
        self.synchronize(state);
        if let Some(index) = state
            .player_company
            .fleet
            .trains
            .iter()
            .position(|train| train.id == train_id)
        {
            self.select_index(state, index);
        }
    }

    /// Returns the selected Train after reconciling a changed Fleet.
    pub fn selected_train_id(&mut self, state: &GameState) -> Option<TrainId> {
        self.synchronize(state);
        self.selected_train_id
    }

    /// Moves the selected Train in response to the Fleet browse controls.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let train_count = state.player_company.fleet.trains.len();
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(train_count.saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(train_count.saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    fn synchronize(&mut self, state: &GameState) {
        let trains = &state.player_company.fleet.trains;
        let previous_index = self.table_state.selected().unwrap_or(0);
        let selected = self
            .selected_train_id
            .and_then(|train_id| trains.iter().position(|train| train.id == train_id))
            .or_else(|| {
                (!trains.is_empty()).then_some(previous_index.min(trains.len().saturating_sub(1)))
            });
        if let Some(index) = selected {
            self.selected_train_id = Some(trains[index].id);
        } else {
            self.selected_train_id = None;
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(train) = state.player_company.fleet.trains.get(index) else {
            return;
        };
        self.selected_train_id = Some(train.id);
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

/// The result of handling a key within the resale flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FleetFlowAction {
    /// The player is still reviewing a possible resale.
    Continue,
    /// The player abandoned the resale without changing the Fleet.
    Cancel,
    /// The application boundary must revalidate and sell the selected Train.
    Confirm { train_id: TrainId },
}

/// Presentation state for one uncommitted Train resale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetFlow {
    train_id: TrainId,
    rejection: Option<String>,
}

impl FleetFlow {
    /// Starts reviewing the currently selected READY Train without changing the Fleet.
    pub fn start(state: &GameState, train_id: TrainId) -> Result<Self, String> {
        resale_review(state, train_id)?;
        Ok(Self {
            train_id,
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> FleetFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return FleetFlowAction::Cancel;
        }

        if !matches!(key.code, KeyCode::Enter) {
            return FleetFlowAction::Continue;
        }
        match resale_review(state, self.train_id) {
            Ok(_) => FleetFlowAction::Confirm {
                train_id: self.train_id,
            },
            Err(reason) => {
                self.rejection = Some(reason);
                FleetFlowAction::Continue
            }
        }
    }

    /// Records an application-boundary rejection while keeping the proposal visible.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the resale review as plain text for legacy textual callers.
    pub fn render(&self, state: &GameState, now: UtcSeconds) -> String {
        let mut output = String::new();
        match resale_review(state, self.train_id) {
            Ok(review) => {
                writeln!(
                    output,
                    "\nResell Train {:02} — {}",
                    review.train.id.get(),
                    train_model_name(review.train)
                )
                .expect("writing to a String cannot fail");
                writeln!(
                    output,
                    "Sale proceeds (70%): {}",
                    format_money(review.proceeds)
                )
                .expect("writing to a String cannot fail");
                writeln!(
                    output,
                    "Company Funds after resale: {}",
                    format_money(review.funds_after)
                )
                .expect("writing to a String cannot fail");
            }
            Err(reason) => writeln!(output, "\nResale unavailable: {reason}")
                .expect("writing to a String cannot fail"),
        }
        if let Some(rejection) = &self.rejection {
            writeln!(output, "Resale rejected: {rejection}")
                .expect("writing to a String cannot fail");
        }
        writeln!(output, "\n{}", render_at(state, now)).expect("writing to a String cannot fail");
        output
    }

    /// Renders the full review inside the Fleet workspace.
    pub fn render_review(&self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_resale_review(frame, area, state, self);
    }
}

struct ResaleReview<'a> {
    train: &'a Train,
    proceeds: Money,
    funds_after: Money,
}

fn resale_review(state: &GameState, train_id: TrainId) -> Result<ResaleReview<'_>, String> {
    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .ok_or_else(|| format!("Train {} is no longer in the Fleet.", train_id.get()))?;
    if matches!(train.status, TrainStatus::Travelling { .. }) {
        return Err(format!(
            "Train {} is TRAVELLING and cannot be resold until its Journey arrives.",
            train.id.get()
        ));
    }
    let proceeds = resale_proceeds(train.original_purchase_price).ok_or_else(|| {
        format!(
            "Train {} has no valid original purchase price for resale.",
            train.id.get()
        )
    })?;
    let funds_after = state
        .player_company
        .funds
        .checked_add(proceeds)
        .map_err(|_| {
            String::from("Company Funds cannot represent the resale result; no resale was saved.")
        })?;
    Ok(ResaleReview {
        train,
        proceeds,
        funds_after,
    })
}

fn render_resale_review(frame: &mut Frame, area: Rect, state: &GameState, flow: &FleetFlow) {
    let block = panel_block("Trains · resale review", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match resale_review(state, flow.train_id) {
        Ok(review) => vec![
            Line::styled(
                format!("Resell Train {:02}", review.train.id.get()),
                theme::title(),
            ),
            labelled_line("Model", &train_model_name(review.train)),
            labelled_line("Status", "READY"),
            labelled_line(
                "Proceeds",
                &format!("{} (70%)", format_money(review.proceeds)),
            ),
            labelled_line("Funds now", &format_money(state.player_company.funds)),
            labelled_line("Funds after", &format_money(review.funds_after)),
            Line::from(""),
            Line::styled("Enter · confirm resale", theme::focused_title()),
            Line::styled("Esc · cancel (no changes)", theme::hint()),
        ],
        Err(reason) => vec![
            Line::styled("Resale unavailable", theme::title()),
            Line::styled(reason, theme::error()),
            Line::from(""),
            Line::styled("Esc · return to Fleet", theme::hint()),
        ],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );

    if let Some(rejection) = &flow.rejection {
        let rejection_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(inner.height.saturating_sub(2)),
            width: inner.width,
            height: inner.height.min(2),
        };
        frame.render_widget(
            Paragraph::new(vec![Line::styled(
                format!("Resale rejected: {rejection}"),
                theme::error(),
            )])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            rejection_area,
        );
    }
}

/// Renders the Fleet workspace. Wide terminals keep the selected Train's
/// operational inspector visible at all times; compact terminals use Enter to
/// replace the list with the selected Train's detail page.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
    details_open: bool,
) {
    selection.synchronize(state);
    if state.player_company.fleet.trains.is_empty() {
        frame.render_widget(empty_fleet_panel(state), area);
        return;
    }

    if area.width >= 96 && area.height >= 18 {
        render_wide_dashboard(frame, area, state, now, selection);
    } else if details_open {
        render_compact_details(frame, area, state, now, selection);
    } else {
        render_compact_dashboard(frame, area, state, now, selection);
    }
}

fn render_wide_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
) {
    let [table_area, inspector_area] =
        Layout::horizontal([Constraint::Min(58), Constraint::Length(38)])
            .spacing(1)
            .areas(area);
    let visible_items = usize::from(table_area.height.saturating_sub(4)).max(1);
    selection.set_page_size(visible_items);

    let rows = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| {
            let fields = train_fields(state, train, now);
            Row::new([
                Cell::from(format!("{:02}", train.id.get())),
                Cell::from(fields.model),
                Cell::from(fields.status).style(train_status_style(train)),
                Cell::from(fields.place),
                Cell::from(fields.eta),
            ])
        })
        .collect::<Vec<_>>();
    let header = Row::new(["#", "Model", "State", "Position", "ETA"])
        .style(theme::table_header())
        .bottom_margin(1);
    let title = fleet_title(state);
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Percentage(27),
            Constraint::Length(10),
            Constraint::Percentage(43),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(panel_block(&title, true))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol(theme::SELECTION_MARKER)
    .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut selection.table_state);

    let selected = selected_train(state, selection);
    render_train_inspector(frame, inspector_area, state, now, selected, false);
}

fn render_compact_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
) {
    let visible_items = usize::from(area.height.saturating_sub(2) / 2).max(1);
    selection.set_page_size(visible_items);
    selection.keep_compact_selection_visible(visible_items);
    let offset = selection.table_state.offset();
    let selected = selection.table_state.selected();
    let lines = state
        .player_company
        .fleet
        .trains
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_items)
        .flat_map(|(index, train)| compact_train_lines(state, train, now, selected == Some(index)))
        .collect::<Vec<_>>();
    let title = fleet_title(state);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(&title, true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_compact_details(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
) {
    let selected = selected_train(state, selection);
    render_train_inspector(frame, area, state, now, selected, true);
}

fn empty_fleet_panel(state: &GameState) -> Paragraph<'static> {
    let mut lines = vec![
        Line::styled("No Trains yet", theme::title()),
        Line::from("Your company owns no rolling stock."),
    ];
    let has_delivery_station = !state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .is_empty();
    let affordable = has_delivery_station
        && train_catalogue()
            .models()
            .iter()
            .any(|train| state.player_company.funds >= train.purchase_price());
    lines.push(Line::from(""));
    lines.push(Line::styled(
        if affordable {
            "Next useful action · 3 Market"
        } else {
            "No catalogue Train is currently affordable. Review Company Funds."
        },
        theme::hint(),
    ));
    Paragraph::new(lines)
        .block(panel_block("Trains", true))
        .style(theme::panel())
}

fn selected_train<'a>(state: &'a GameState, selection: &mut FleetSelection) -> Option<&'a Train> {
    selection.selected_train_id(state).and_then(|train_id| {
        state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == train_id)
    })
}

fn render_train_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selected: Option<&Train>,
    compact_detail: bool,
) {
    let Some(train) = selected else {
        frame.render_widget(
            Paragraph::new("No Train selected")
                .block(panel_block("Train", compact_detail))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let fields = train_fields(state, train, now);
    let title = format!("Train {:02} · {}", train.id.get(), fields.model);
    let block = panel_block(&title, compact_detail);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let journey = journey_for_train(train, state);
    let gauge_height = if journey.is_some() { 1 } else { 0 };
    let [details_area, progress_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(gauge_height)]).areas(inner);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("STATE         ", theme::secondary()),
            Span::styled(train_status_label(train), train_status_style(train)),
        ]),
        Line::from(""),
        section_heading("OPERATIONS"),
        labelled_line("Position", &fields.place),
    ];

    if let Some(journey) = journey {
        lines.push(labelled_line(
            "ETA",
            &format_duration(remaining_seconds(journey, now)),
        ));
        lines.push(labelled_line(
            "Progress",
            &format!("{}%", journey_progress_percent(journey, now)),
        ));
    } else {
        lines.push(labelled_line("Availability", "Ready for dispatch"));
    }

    lines.extend([
        Line::from(""),
        section_heading("SPECIFICATIONS"),
        labelled_line(
            "Capacity",
            &format_capacity(train),
        ),
        labelled_line("Top speed", &format_speed(train)),
        labelled_line("Fuel", &format_fuel_rate(train)),
        labelled_line("Paid", &format_money(train.original_purchase_price)),
        labelled_line(
            "Resale",
            &resale_proceeds(train.original_purchase_price)
                .map(format_money)
                .unwrap_or_else(|| "Unavailable".into()),
        ),
        Line::from(""),
        section_heading("ACTIONS"),
        Line::styled(train_action_line(train, compact_detail), theme::hint()),
    ]);

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        details_area,
    );

    if let Some(journey) = journey {
        let percent = journey_progress_percent(journey, now).min(100);
        frame.render_widget(
            LineGauge::default()
                .ratio(percent as f64 / 100.0)
                .label(format!("{percent}%"))
                .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
                .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
            progress_area,
        );
    }
}

fn fleet_title(state: &GameState) -> String {
    let ready = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .count();
    let en_route = state.player_company.fleet.trains.len().saturating_sub(ready);
    format!(
        "Trains · {} total · {} READY · {} TRAVELLING",
        state.player_company.fleet.trains.len(),
        ready,
        en_route
    )
}

fn train_status_label(train: &Train) -> &'static str {
    match &train.status {
        TrainStatus::Ready { .. } => "READY",
        TrainStatus::Travelling { .. } => "TRAVELLING",
    }
}

fn train_status_style(train: &Train) -> Style {
    match &train.status {
        TrainStatus::Ready { .. } => Style::default().fg(theme::SUCCESS),
        TrainStatus::Travelling { .. } => Style::default().fg(theme::ACCENT),
    }
}

fn train_action_line(train: &Train, compact_detail: bool) -> &'static str {
    match (&train.status, compact_detail) {
        (TrainStatus::Ready { .. }, true) => "d Dispatch   s Resale   Esc Back",
        (TrainStatus::Ready { .. }, false) => "d Dispatch   s Resale",
        (TrainStatus::Travelling { .. }, true) => {
            "No actions until arrival   Esc Back"
        }
        (TrainStatus::Travelling { .. }, false) => "No actions available until arrival",
    }
}

fn journey_for_train<'a>(train: &Train, state: &'a GameState) -> Option<&'a Journey> {
    let TrainStatus::Travelling { journey_id } = &train.status else {
        return None;
    };
    state
        .active_journeys
        .iter()
        .find(|journey| journey.id == *journey_id)
}

fn compact_train_lines(
    state: &GameState,
    train: &Train,
    now: UtcSeconds,
    selected: bool,
) -> [Line<'static>; 2] {
    let fields = train_fields(state, train, now);
    let marker = if selected { ">" } else { " " };
    let row_style = selected
        .then(theme::selected_row)
        .unwrap_or_else(theme::panel);
    [
        Line::styled(
            format!(
                "{marker} {:02}  {} · {}",
                train.id.get(),
                fields.model,
                train_status_label(train)
            ),
            row_style.add_modifier(Modifier::BOLD),
        ),
        Line::styled(format!("  {} · {}", fields.place, fields.eta), row_style),
    ]
}

fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<13}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
}

fn section_heading(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::table_header())
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

struct TrainFields {
    model: String,
    status: String,
    place: String,
    eta: String,
}

fn train_fields(state: &GameState, train: &Train, now: UtcSeconds) -> TrainFields {
    let model = train_model_name(train);
    match &train.status {
        TrainStatus::Ready { at } => TrainFields {
            model,
            status: "READY".into(),
            place: format!("At {}", station_label_or_missing(state, *at)),
            eta: "—".into(),
        },
        TrainStatus::Travelling { journey_id } => {
            let Some(journey) = state
                .active_journeys
                .iter()
                .find(|journey| journey.id == *journey_id)
            else {
                return TrainFields {
                    model,
                    status: "TRAVELLING".into(),
                    place: format!("Journey {} details missing", journey_id.get()),
                    eta: "Unavailable".into(),
                };
            };
            TrainFields {
                model,
                status: "TRAVELLING".into(),
                place: format!(
                    "{} → {}",
                    station_label_or_missing(state, journey.origin_station_id),
                    station_label_or_missing(state, journey.destination_station_id),
                ),
                eta: format!("in {}", format_duration(remaining_seconds(journey, now))),
            }
        }
    }
}

fn station_label_or_missing(state: &GameState, station_id: RailStationId) -> String {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return format!("Missing Rail Station {}", station_id.get());
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| format!("Missing Settlement {}", station.settlement_id.get()))
}

fn train_model_name(train: &Train) -> String {
    model_for_train(train)
        .map(|model| model.name().to_owned())
        .unwrap_or_else(|| format!("Unknown model ({})", train.model_id.as_str()))
}

fn format_capacity(train: &Train) -> String {
    model_for_train(train)
        .map(|model| format!("{} passengers", model.passenger_capacity().passengers()))
        .unwrap_or_else(|| "Unavailable".into())
}

fn format_speed(train: &Train) -> String {
    model_for_train(train)
        .map(|model| crate::ui::format::speed_kmh(model.speed().metres_per_second()))
        .unwrap_or_else(|| "Unavailable".into())
}

fn format_fuel_rate(train: &Train) -> String {
    let Some(model) = model_for_train(train) else {
        return "Unavailable".into();
    };
    let cents = i64::try_from(model.fuel_cost_per_kilometre().cents_per_kilometre())
        .unwrap_or(i64::MAX);
    format!("{}/km", format_money(Money::from_cents(cents)))
}

/// Renders the Player Company's Fleet at the supplied time.
pub fn render_at(state: &GameState, now: UtcSeconds) -> String {
    let mut output = String::from("Trains\n");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");

    if state.player_company.fleet.trains.is_empty() {
        writeln!(
            output,
            "\nNo Trains yet. Next useful action: 3 · Market."
        )
        .expect("writing to a String cannot fail");
        return output;
    }

    for train in &state.player_company.fleet.trains {
        match train.status {
            TrainStatus::Ready { at } => {
                writeln!(
                    output,
                    "\nTrain {} — {}\n  READY at {}\n  Eligible sale proceeds: {} (70% of original purchase price)",
                    train.id.get(),
                    train_model_name(train),
                    station_label(state, at),
                    resale_proceeds(train.original_purchase_price)
                        .map(format_money)
                        .unwrap_or_else(|| "unavailable".into()),
                )
                .expect("writing to a String cannot fail");
            }
            TrainStatus::Travelling { journey_id } => {
                let journey = state
                    .active_journeys
                    .iter()
                    .find(|journey| journey.id == journey_id);
                match journey {
                    Some(journey) => writeln!(
                        output,
                        "\nTrain {} — {}\n  TRAVELLING {} -> {} | progress: {}% | ETA: {}\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train_model_name(train),
                        station_label(state, journey.origin_station_id),
                        station_label(state, journey.destination_station_id),
                        journey_progress_percent(journey, now),
                        format_duration(remaining_seconds(journey, now)),
                    )
                    .expect("writing to a String cannot fail"),
                    None => writeln!(
                        output,
                        "\nTrain {} — {}\n  TRAVELLING on Journey {} (details unavailable)\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train_model_name(train),
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }
    writeln!(
        output,
        "\nPress S to review resale for the selected READY Train."
    )
    .expect("writing to a String cannot fail");
    if state
        .player_company
        .fleet
        .trains
        .iter()
        .all(|train| matches!(train.status, TrainStatus::Travelling { .. }))
    {
        if let Some(eta) = nearest_arrival(state, now) {
            writeln!(
                output,
                "Next useful action: wait for the nearest Train arrival (ETA {eta})."
            )
            .expect("writing to a String cannot fail");
        }
    } else {
        writeln!(output, "READY Trains can enter Manual Dispatch with D.")
            .expect("writing to a String cannot fail");
    }
    output
}

fn resale_proceeds(original_purchase_price: Money) -> Option<Money> {
    (original_purchase_price.cents() > 0)
        .then_some(original_purchase_price)
        .and_then(|price| {
            price
                .cents()
                .checked_mul(70)
                .map(|cents| Money::from_cents(cents / 100))
        })
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

fn nearest_arrival(state: &GameState, now: UtcSeconds) -> Option<String> {
    state
        .active_journeys
        .iter()
        .map(|journey| remaining_seconds(journey, now))
        .min()
        .map(format_duration)
}

fn format_duration(seconds: u64) -> String {
    crate::ui::format::duration(seconds)
}

fn format_money(money: Money) -> String {
    crate::ui::format::money(money)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{RailStationId, UtcSeconds},
        sim::{
            fleet::{purchase_train, sell_train},
            journeys::dispatch_journey,
            services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{FleetFlow, FleetFlowAction, render_at};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn fleet_shows_ready_location_and_confirmed_resale_proceeds() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let rendered = render_at(&state, STARTED_AT);

        let station = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .find(|station| station.id == RailStationId::new(1))
            .unwrap();
        let settlement = state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == station.settlement_id)
            .unwrap();
        assert!(rendered.contains(&format!("READY at {}", settlement.name)));
        assert!(rendered.contains("Eligible sale proceeds:"));

        let mut flow = FleetFlow::start(&state, train_id).unwrap();
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            FleetFlowAction::Confirm { train_id }
        );
        assert!(flow.render(&state, STARTED_AT).contains("sale proceeds"));
        assert!(sell_train(&mut state, train_id).is_ok());
    }

    #[test]
    fn travelling_train_shows_progress_eta_and_cannot_enter_resale_flow() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(2);
        let train_id = purchase_train(&mut state, 0, origin).unwrap();
        let service_id = find_or_create_service(&mut state, origin, destination).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let journey = &state.active_journeys[0];
        let halfway = UtcSeconds::from_unix_seconds(
            journey.departed_at.unix_seconds()
                + (journey.arrives_at.unix_seconds() - journey.departed_at.unix_seconds()) / 2,
        );

        let rendered = render_at(&state, halfway);
        assert!(rendered.contains("TRAVELLING"));
        assert!(rendered.contains("progress: 50%"));
        assert!(rendered.contains("ETA:"));
        assert!(rendered.contains("Sale unavailable"));
        assert!(FleetFlow::start(&state, train_id).is_err());
        assert!(matches!(
            sell_train(&mut state, train_id),
            Err(crate::sim::fleet::FleetError::TrainTravelling { .. })
        ));
    }
}
