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
                    review.train.model_name
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
    let block = panel_block("Fleet · resale review", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match resale_review(state, flow.train_id) {
        Ok(review) => vec![
            Line::styled(
                format!("Resell Train {:02}", review.train.id.get()),
                theme::title(),
            ),
            labelled_line("Model", &review.train.model_name),
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

/// Renders the Fleet browser as a stateful table when there is room, falling
/// back to labelled stacked Train rows in compact workspaces.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
    details_open: bool,
    details_focused: bool,
) {
    selection.synchronize(state);
    if state.player_company.fleet.trains.is_empty() {
        frame.render_widget(empty_fleet_panel(), area);
        return;
    }

    if area.width >= 96 && area.height >= 14 {
        render_wide_dashboard(
            frame,
            area,
            state,
            now,
            selection,
            details_open,
            details_focused,
        );
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
    details_open: bool,
    details_focused: bool,
) {
    let [table_area, inspector_area] =
        Layout::horizontal([Constraint::Min(58), Constraint::Length(32)])
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
                Cell::from(format!("Train {:02}", train.id.get())),
                Cell::from(fields.model),
                Cell::from(fields.status),
                Cell::from(fields.place),
                Cell::from(fields.eta),
            ])
        })
        .collect::<Vec<_>>();
    let header = Row::new(["Train", "Model", "Status", "Station / destination", "ETA"])
        .style(theme::table_header())
        .bottom_margin(1);
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(26),
            Constraint::Length(12),
            Constraint::Percentage(40),
            Constraint::Length(11),
        ],
    )
    .header(header)
    .block(panel_block("Fleet · owned Trains", !details_focused))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("> ")
    .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut selection.table_state);
    render_inspector(
        frame,
        inspector_area,
        state,
        now,
        selection,
        details_open,
        details_focused,
    );
}

fn render_compact_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
) {
    let visible_items = usize::from(area.height.saturating_sub(2) / 3).max(1);
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
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Fleet · owned Trains", true))
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
    render_train_details(frame, area, state, now, selected, true);
}

fn empty_fleet_panel() -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::styled("No Trains in the Fleet", theme::title()),
        Line::from("Buy a Train to begin operating. [B] Buy Trains"),
    ])
    .block(panel_block("Fleet · owned Trains", true))
    .style(theme::panel())
}

fn render_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut FleetSelection,
    details_open: bool,
    focused: bool,
) {
    let selected = selected_train(state, selection);
    if details_open {
        render_train_details(frame, area, state, now, selected, focused);
    } else {
        let lines = selected.map_or_else(
            || vec![Line::from("No Train selected")],
            |train| {
                let fields = train_fields(state, train, now);
                vec![
                    Line::styled(format!("Train {:02}", train.id.get()), theme::title()),
                    labelled_line("Model", &fields.model),
                    labelled_line("Status", &fields.status),
                    labelled_line("Location", &fields.place),
                    labelled_line(
                        "Capacity",
                        &format!("{} passengers", train.passenger_capacity.passengers()),
                    ),
                    Line::from(""),
                    Line::styled(
                        format!("Enter · inspect    {}", fleet_action_hint(train)),
                        theme::hint(),
                    ),
                ]
            },
        );
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel_block("Selected Train", false))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
    }
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

fn render_train_details(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selected: Option<&Train>,
    focused: bool,
) {
    let block = panel_block("Train details", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(train) = selected else {
        frame.render_widget(
            Paragraph::new("No Train selected").style(theme::panel()),
            inner,
        );
        return;
    };
    let fields = train_fields(state, train, now);
    let journey = journey_progress(train, state, now);
    let [details_area, progress_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let mut lines = vec![
        Line::styled(format!("Train {:02}", train.id.get()), theme::title()),
        labelled_line("Model", &fields.model),
        labelled_line("Status", &fields.status),
        labelled_line("Location", &fields.place),
        labelled_line(
            "Capacity",
            &format!("{} passengers", train.passenger_capacity.passengers()),
        ),
        labelled_line("Speed", &format_speed(train)),
    ];
    if let Some((percent, remaining)) = journey {
        lines.push(labelled_line("Remaining", &remaining));
        lines.push(Line::from("Journey progress"));
        frame.render_widget(
            LineGauge::default()
                .ratio(f64::from(percent) / 100.0)
                .label(format!("{percent}%"))
                .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
                .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
            progress_area,
        );
    } else {
        lines.push(labelled_line("Remaining", "Not travelling"));
        lines.push(Line::from("No active Journey"));
        frame.render_widget(
            LineGauge::default()
                .ratio(0.0)
                .label("Not travelling")
                .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
                .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
            progress_area,
        );
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        format!("Esc · Fleet list    {}", fleet_action_hint(train)),
        theme::hint(),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        details_area,
    );
}

fn fleet_action_hint(train: &Train) -> &'static str {
    match train.status {
        TrainStatus::Ready { .. } => "D · dispatch    S · resale",
        TrainStatus::Travelling { .. } => {
            "Dispatch / resale unavailable · Train is TRAVELLING until its Journey arrives"
        }
    }
}

fn journey_progress(train: &Train, state: &GameState, now: UtcSeconds) -> Option<(u16, String)> {
    let TrainStatus::Travelling { journey_id } = train.status else {
        return None;
    };
    let journey = state
        .active_journeys
        .iter()
        .find(|journey| journey.id == journey_id)?;
    let percent = u16::try_from(journey_progress_percent(journey, now)).unwrap_or(100);
    Some((percent, format_duration(remaining_seconds(journey, now))))
}

fn compact_train_lines(
    state: &GameState,
    train: &Train,
    now: UtcSeconds,
    selected: bool,
) -> [Line<'static>; 3] {
    let fields = train_fields(state, train, now);
    let marker = if selected { ">" } else { " " };
    let row_style = selected
        .then(theme::selected_row)
        .unwrap_or_else(theme::panel);
    [
        Line::styled(
            format!("{marker} Train {:02}  {}", train.id.get(), fields.status),
            row_style.add_modifier(Modifier::BOLD),
        ),
        Line::styled(format!("  {}", fields.model), row_style),
        Line::styled(format!("  {} · {}", fields.place, fields.eta), row_style),
    ]
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

struct TrainFields {
    model: String,
    status: String,
    place: String,
    eta: String,
}

fn train_fields(state: &GameState, train: &Train, now: UtcSeconds) -> TrainFields {
    let model = if train.model_name.trim().is_empty() {
        "Model unavailable".into()
    } else {
        train.model_name.clone()
    };
    match train.status {
        TrainStatus::Ready { at } => TrainFields {
            model,
            status: "READY".into(),
            place: format!("At {}", station_label_or_missing(state, at)),
            eta: "—".into(),
        },
        TrainStatus::Travelling { journey_id } => {
            let Some(journey) = state
                .active_journeys
                .iter()
                .find(|journey| journey.id == journey_id)
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

fn format_speed(train: &Train) -> String {
    let kilometres_per_hour = train.speed.metres_per_second().saturating_mul(18) / 5;
    format!("{kilometres_per_hour} km/h")
}

/// Renders the Player Company's Fleet at the supplied time.
pub fn render_at(state: &GameState, now: UtcSeconds) -> String {
    let mut output = String::from("Fleet\n");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");

    if state.player_company.fleet.trains.is_empty() {
        writeln!(
            output,
            "\nNo Trains in the Fleet. Buy a Train to begin operating."
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
                    train.model_name,
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
                        train.model_name,
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
                        train.model_name,
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

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    let grouped_whole = (cents / 100)
        .to_string()
        .chars()
        .rev()
        .enumerate()
        .fold(String::new(), |mut output, (index, digit)| {
            if index != 0 && index % 3 == 0 {
                output.push(',');
            }
            output.push(digit);
            output
        })
        .chars()
        .rev()
        .collect::<String>();
    format!("{sign}${grouped_whole}.{:02}", cents % 100)
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
