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
    model::{
        GameState, Journey, Money, RailStationId, Train, TrainId, TrainNickname, TrainStatus,
        UtcSeconds,
    },
    ui::{modal, theme},
};

const FLEET_SELECTION_MARKER: &str = "› ";

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


/// Presentation-only editor for one Train's optional player nickname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrainNicknameEditor {
    train_id: TrainId,
    draft: String,
    error: Option<String>,
}

/// Outcome of one key handled by the Train nickname editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrainNicknameEditorAction {
    Continue,
    Cancel,
    Confirm {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
}

impl TrainNicknameEditor {
    /// Starts editing the selected Train's current nickname.
    pub fn start(state: &GameState, train_id: TrainId) -> Result<Self, String> {
        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == train_id)
            .ok_or_else(|| format!("Train {} is no longer in the Fleet.", train_id.get()))?;
        Ok(Self {
            train_id,
            draft: train
                .nickname
                .as_ref()
                .map(TrainNickname::as_str)
                .unwrap_or_default()
                .to_owned(),
            error: None,
        })
    }

    /// Handles nickname text editing without mutating the saved Train.
    pub fn handle_key(&mut self, key: KeyCode) -> TrainNicknameEditorAction {
        match key {
            KeyCode::Esc => TrainNicknameEditorAction::Cancel,
            KeyCode::Enter => {
                let trimmed = self.draft.trim();
                if trimmed.is_empty() {
                    TrainNicknameEditorAction::Confirm {
                        train_id: self.train_id,
                        nickname: None,
                    }
                } else {
                    match TrainNickname::parse(trimmed) {
                        Ok(nickname) => TrainNicknameEditorAction::Confirm {
                            train_id: self.train_id,
                            nickname: Some(nickname),
                        },
                        Err(error) => {
                            self.error = Some(error.to_string());
                            TrainNicknameEditorAction::Continue
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.draft.pop();
                self.error = None;
                TrainNicknameEditorAction::Continue
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && self.draft.chars().count() < TrainNickname::MAX_CHARACTERS =>
            {
                self.draft.push(character);
                self.error = None;
                TrainNicknameEditorAction::Continue
            }
            KeyCode::Char(_) => {
                self.error = Some(format!(
                    "Nickname accepts up to {} visible characters.",
                    TrainNickname::MAX_CHARACTERS
                ));
                TrainNicknameEditorAction::Continue
            }
            _ => TrainNicknameEditorAction::Continue,
        }
    }

    pub fn train_id(&self) -> TrainId {
        self.train_id
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Renders the Train nickname editor as a focused modal above the Fleet workspace.
pub fn render_nickname_editor(
    frame: &mut Frame,
    area: Rect,
    editor: &TrainNicknameEditor,
    state: &GameState,
) {
    let card_height = if editor.error().is_some() { 15 } else { 14 };
    let card = modal::centered_rect(area, 68, card_height);
    let footer = if card.width >= 56 {
        modal::shortcut_line(&[("Enter", "save"), ("Backspace", "delete"), ("Esc", "cancel")])
    } else {
        modal::shortcut_line(&[("Enter", "save"), ("Esc", "cancel")])
    };
    let modal_areas = modal::render_shell(frame, card, "Rename Train", footer);

    let train = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == editor.train_id());
    let (model, evn) = train.map_or_else(
        || ("Unknown Train".to_owned(), "Unavailable".to_owned()),
        |train| (train_model_name(train), train.evn.formatted()),
    );

    let draft = if editor.draft().is_empty() {
        " ".to_owned()
    } else {
        editor.draft().to_owned()
    };
    let mut lines = vec![
        Line::styled(
            format!("Train {:02} · {model}", editor.train_id().get()),
            theme::focused_title(),
        ),
        labelled_line("EVN", &evn),
        Line::from(""),
        section_heading("NICKNAME"),
        Line::from(vec![
            Span::styled("› ", theme::focused_title()),
            Span::styled(draft, theme::primary_value()),
            Span::styled("▏", theme::focused_title()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "This is a player-facing label only; the official EVN never changes.",
            theme::secondary(),
        )),
        Line::from(Span::styled(
            if editor.draft().is_empty() {
                "Saving an empty nickname restores the default Train label."
            } else {
                "Type to edit the nickname; Backspace removes the previous character."
            },
            theme::secondary(),
        )),
    ];
    if let Some(error) = editor.error() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(error.to_owned(), theme::error())));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
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
    let review = resale_review(state, flow.train_id);
    let card = modal::centered_rect(area, 64, 16);
    let footer = if review.is_ok() {
        modal::shortcut_line(&[("Enter", "resell"), ("Esc", "cancel")])
    } else {
        modal::shortcut_line(&[("Esc", "close")])
    };
    let modal_areas = modal::render_shell(frame, card, "Confirm Train Resale", footer);

    let mut lines = match review {
        Ok(review) => vec![
            Line::styled(
                format!("Resell Train {:02}", review.train.id.get()),
                theme::focused_title(),
            ),
            labelled_line("Model", &train_model_name(review.train)),
            labelled_line("Status", "READY"),
            Line::from(""),
            labelled_line(
                "Proceeds",
                &format!("{} (70%)", format_money(review.proceeds)),
            ),
            labelled_line("Funds now", &format_money(state.player_company.funds)),
            labelled_line("Funds after", &format_money(review.funds_after)),
            Line::from(""),
            Line::styled(
                "The Train will be removed from your Fleet after the sale is saved.",
                theme::warning(),
            ),
        ],
        Err(reason) => vec![
            Line::styled("Resale unavailable", theme::error()),
            Line::from(""),
            Line::from(reason),
        ],
    };
    if let Some(rejection) = &flow.rejection {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Resale rejected: {rejection}"),
            theme::error(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

/// Renders the Fleet workspace. Wide terminals use one focused Fleet frame
/// with a Train picker on the left and the selected Train inspector on the
/// right. Compact terminals keep the same selection language and use Enter to
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
    // The Fleet is one workspace, not two neighbouring windows. A single
    // focused frame owns the view and a quiet divider separates selection
    // from inspection, mirroring the picker/detail rhythm used by modals.
    let shell = panel_block("Fleet", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let [list_area, inspector_area] = Layout::horizontal([
        Constraint::Min(48),
        Constraint::Length(44),
    ])
    .areas(shell_inner);
    let list_area = horizontal_inset(list_area, 1);
    let visible_items = usize::from(list_area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);

    let rows = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| {
            let fields = train_fields(state, train, now);
            Row::new([
                Cell::from(train_picker_label(train, &fields.model)),
                Cell::from(fields.status).style(train_status_style(train)),
            ])
        })
        .collect::<Vec<_>>();
    let header = Row::new(["Train", "State"])
        .style(theme::table_header())
        .bottom_margin(1);
    let table = Table::new(
        rows,
        [Constraint::Min(24), Constraint::Length(11)],
    )
    .header(header)
    .style(theme::panel())
    .row_highlight_style(theme::selected_row())
    .highlight_symbol(FLEET_SELECTION_MARKER)
    .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, list_area, &mut selection.table_state);

    let selected = selected_train(state, selection);
    render_train_inspector(frame, inspector_area, state, now, selected, false, true);
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
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Fleet", true))
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
    render_train_inspector(frame, area, state, now, selected, true, false);
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
            "Purchase your first Train from the Market."
        } else {
            "No catalogue Train is currently affordable. Review Company Funds."
        },
        theme::hint(),
    ));
    Paragraph::new(lines)
        .block(panel_block("Fleet", true))
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
    embedded: bool,
) {
    let Some(train) = selected else {
        let block = if embedded {
            Block::default()
                .borders(Borders::LEFT)
                .border_style(theme::border())
                .style(theme::panel())
        } else {
            panel_block("Train", compact_detail)
        };
        frame.render_widget(
            Paragraph::new("No Train selected")
                .block(block)
                .style(theme::panel()),
            area,
        );
        return;
    };

    let fields = train_fields(state, train, now);
    let title = train
        .nickname
        .as_ref()
        .map(|nickname| format!("{} · Train {:02}", nickname.as_str(), train.id.get()))
        .unwrap_or_else(|| format!("Train {:02}", train.id.get()));
    let inner = if embedded {
        let divider = Block::default()
            .borders(Borders::LEFT)
            .border_style(theme::border())
            .style(theme::panel());
        let inner = horizontal_inset(divider.inner(area), 1);
        frame.render_widget(divider, area);
        inner
    } else {
        let block = panel_block(&title, compact_detail);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    };

    let journey = journey_for_train(train, state);
    let gauge_height = if journey.is_some() { 1 } else { 0 };
    let [details_area, progress_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(gauge_height)]).areas(inner);

    let mut lines = Vec::new();
    if embedded {
        lines.push(Line::styled(title.clone(), theme::focused_title()));
        lines.push(Line::styled(fields.model.clone(), theme::secondary()));
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(vec![
            Span::styled(fields.model.clone(), theme::primary_value()),
            Span::styled(" · ", theme::secondary()),
            Span::styled(train.evn.formatted(), theme::secondary()),
        ]));
    }

    if !compact_detail {
        lines.extend([
            section_heading("IDENTITY"),
            labelled_line("EVN", &train.evn.formatted()),
            labelled_line(
                "Keeper mark",
                &format!(
                    "{}-{}",
                    state.region.railway_registration.mark,
                    state.player_company.vehicle_keeper_mark.as_str(),
                ),
            ),
            Line::from(""),
        ]);
    }

    lines.push(section_heading("STATUS"));
    lines.push(labelled_line_styled(
        "State",
        train_status_label(train),
        train_status_style(train),
    ));
    match (&train.status, journey) {
        (TrainStatus::Ready { .. }, _) => {
            lines.push(labelled_line("Availability", "Ready for dispatch"));
        }
        (TrainStatus::Travelling { .. }, Some(_)) => {
            lines.push(labelled_line("Availability", "In service"));
        }
        (TrainStatus::Travelling { .. }, None) => {
            lines.push(labelled_line("Availability", "Journey data unavailable"));
        }
    }

    match (&train.status, journey) {
        (TrainStatus::Ready { at }, _) => {
            inspector_section(&mut lines, "LOCATION", compact_detail);
            lines.push(labelled_line(
                "Station",
                &station_label_or_missing(state, *at),
            ));
        }
        (TrainStatus::Travelling { .. }, Some(journey)) => {
            inspector_section(&mut lines, "JOURNEY", compact_detail);
            lines.push(labelled_line(
                "Current leg",
                &journey_leg_label(state, journey),
            ));
            lines.push(labelled_line(
                "Remaining",
                &format_duration(remaining_seconds(journey, now)),
            ));
            lines.push(labelled_line(
                "Journey progress",
                &format!("{}%", journey_progress_percent(journey, now)),
            ));

            inspector_section(&mut lines, "SERVICE", compact_detail);
            if let Some(service) = state
                .player_company
                .passenger_services
                .iter()
                .find(|service| service.id == journey.service_id)
            {
                lines.push(labelled_line("Service", &service.name));
                lines.push(labelled_line(
                    "Route",
                    &format!(
                        "{} → {}",
                        station_label_or_missing(state, journey.origin_station_id),
                        station_label_or_missing(state, journey.destination_station_id),
                    ),
                ));
                if !compact_detail {
                    lines.push(labelled_line("Calls", &service.stop_station_ids.len().to_string()));
                }
            } else {
                lines.push(labelled_line(
                    "Service",
                    &format!("Missing service {}", journey.service_id.get()),
                ));
            }
        }
        (TrainStatus::Travelling { journey_id }, None) => {
            inspector_section(&mut lines, "JOURNEY", compact_detail);
            lines.push(labelled_line("Journey", &journey_id.get().to_string()));
            lines.push(labelled_line("Details", "Unavailable"));
        }
    }

    inspector_section(&mut lines, "CAPACITY", compact_detail);
    lines.push(labelled_line("Seats", &format_capacity(train)));
    if let Some(journey) = journey {
        let onboard = journey.onboard_passengers();
        lines.push(labelled_line("On board", &onboard.to_string()));
        lines.push(labelled_line("Load", &format_load(train, onboard)));
    }

    inspector_section(&mut lines, "PERFORMANCE", compact_detail);
    if compact_detail {
        lines.push(Line::from(vec![
            Span::styled("Technical    ", theme::secondary()),
            Span::raw(format!(
                "{} · {} · {}",
                format_speed(train),
                format_propulsion(train),
                format_fuel_rate(train),
            )),
        ]));
    } else {
        lines.push(labelled_line("Top speed", &format_speed(train)));
        lines.push(labelled_line("Propulsion", &format_propulsion(train)));
        lines.push(labelled_line("Fuel cost", &format_fuel_rate(train)));
    }

    if matches!(&train.status, TrainStatus::Ready { .. }) {
        inspector_section(&mut lines, "VALUE", compact_detail);
        if !compact_detail {
            lines.push(labelled_line(
                "Paid",
                &format_money(train.original_purchase_price),
            ));
        }
        lines.push(labelled_line(
            "Resale",
            &resale_proceeds(train.original_purchase_price)
                .map(format_money)
                .unwrap_or_else(|| "Unavailable".into()),
        ));
    }

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

fn inspector_section(lines: &mut Vec<Line<'static>>, title: &str, compact: bool) {
    if !compact {
        lines.push(Line::from(""));
    }
    lines.push(section_heading(title));
}

fn journey_leg_label(state: &GameState, journey: &Journey) -> String {
    let from = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)
        .and_then(|service| service.stop_station_ids.get(journey.current_stop_index))
        .copied()
        .unwrap_or(journey.origin_station_id);
    let to = journey_next_stop_station_id(state, journey).unwrap_or(journey.destination_station_id);
    format!(
        "{} → {}",
        station_label_or_missing(state, from),
        station_label_or_missing(state, to),
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
    let marker = if selected { "›" } else { " " };
    let row_style = selected
        .then(theme::selected_row)
        .unwrap_or_else(theme::panel);
    let nickname = train
        .nickname
        .as_ref()
        .map(|nickname| format!(" · {}", nickname.as_str()))
        .unwrap_or_default();
    [
        Line::styled(
            format!(
                "{marker} Train {:02}{nickname}  {}",
                train.id.get(),
                train_status_label(train)
            ),
            row_style.add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!("  {} · {} · {}", fields.model, fields.place, fields.eta),
            row_style,
        ),
    ]
}

fn train_picker_label(train: &Train, model: &str) -> String {
    let identity = train
        .nickname
        .as_ref()
        .map(TrainNickname::as_str)
        .unwrap_or(model);
    format!("Train {:02} · {identity}", train.id.get())
}

fn horizontal_inset(area: Rect, amount: u16) -> Rect {
    let inset = amount.min(area.width / 2);
    Rect::new(
        area.x.saturating_add(inset),
        area.y,
        area.width.saturating_sub(inset.saturating_mul(2)),
        area.height,
    )
}

fn labelled_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<13}"), theme::secondary()),
        Span::raw(value.to_owned()),
    ])
}

fn labelled_line_styled(label: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<13}"), theme::secondary()),
        Span::styled(value.to_owned(), style),
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
                place: journey_next_stop_station_id(state, journey)
                    .map(|station_id| format!("Next {}", station_label_or_missing(state, station_id)))
                    .unwrap_or_else(|| {
                        format!(
                            "{} → {}",
                            station_label_or_missing(state, journey.origin_station_id),
                            station_label_or_missing(state, journey.destination_station_id),
                        )
                    }),
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

fn format_load(train: &Train, onboard: u32) -> String {
    let Some(capacity) =
        model_for_train(train).map(|model| model.passenger_capacity().passengers())
    else {
        return "Unavailable".into();
    };
    if capacity == 0 {
        return "—".into();
    }
    format!("{}%", onboard.saturating_mul(100) / capacity)
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

fn format_propulsion(train: &Train) -> String {
    model_for_train(train)
        .map(|model| model.propulsion_label().to_owned())
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
                    "\nTrain {} — {}\n  Nickname {}\n  EVN {}\n  READY at {}\n  Eligible sale proceeds: {} (70% of original purchase price)",
                    train.id.get(),
                    train_model_name(train),
                    train.nickname.as_ref().map(TrainNickname::as_str).unwrap_or("—"),
                    train.evn.formatted(),
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
                        "\nTrain {} — {}\n  Nickname {}\n  EVN {}\n  TRAVELLING {} -> {} | next: {} | leg: {}% | ETA: {} | onboard: {}\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train_model_name(train),
                        train.nickname.as_ref().map(TrainNickname::as_str).unwrap_or("—"),
                        train.evn.formatted(),
                        station_label(state, journey.origin_station_id),
                        station_label(state, journey.destination_station_id),
                        journey_next_stop_station_id(state, journey)
                            .map(|station_id| station_label(state, station_id))
                            .unwrap_or("unknown Rail Station"),
                        journey_progress_percent(journey, now),
                        format_duration(remaining_seconds(journey, now)),
                        journey.onboard_passengers(),
                    )
                    .expect("writing to a String cannot fail"),
                    None => writeln!(
                        output,
                        "\nTrain {} — {}\n  Nickname {}\n  EVN {}\n  TRAVELLING on Journey {} (details unavailable)\n  Sale unavailable while this Journey is in transit.",
                        train.id.get(),
                        train_model_name(train),
                        train.nickname.as_ref().map(TrainNickname::as_str).unwrap_or("—"),
                        train.evn.formatted(),
                        journey_id.get(),
                    )
                    .expect("writing to a String cannot fail"),
                }
            }
        }
    }
    writeln!(
        output,
        "\nPress N to rename the selected Train. Press S to review resale for a READY Train."
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

fn journey_next_stop_station_id(state: &GameState, journey: &Journey) -> Option<RailStationId> {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == journey.service_id)?;
    let first = service.stop_station_ids.first().copied()?;
    let last = service.stop_station_ids.last().copied()?;
    let direction = if journey.origin_station_id == first && journey.destination_station_id == last {
        1_i32
    } else if journey.origin_station_id == last && journey.destination_station_id == first {
        -1_i32
    } else {
        return None;
    };
    let index = if direction > 0 {
        journey.current_stop_index.checked_add(1)?
    } else {
        journey.current_stop_index.checked_sub(1)?
    };
    service.stop_station_ids.get(index).copied()
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

    use super::{
        FleetFlow, FleetFlowAction, TrainNicknameEditor, TrainNicknameEditorAction, render_at,
    };

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn nickname_editor_can_set_and_clear_a_train_name() {
        let mut state = create_new_game(42, "One More Prime", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut editor = TrainNicknameEditor::start(&state, train_id).unwrap();
        for character in "Little Runner".chars() {
            assert_eq!(
                editor.handle_key(KeyCode::Char(character)),
                TrainNicknameEditorAction::Continue
            );
        }
        let action = editor.handle_key(KeyCode::Enter);
        let TrainNicknameEditorAction::Confirm { nickname, .. } = action else {
            panic!("expected nickname confirmation");
        };
        assert_eq!(nickname.unwrap().as_str(), "Little Runner");

        state.player_company.fleet.trains[0].nickname =
            Some(crate::model::TrainNickname::parse("Little Runner").unwrap());
        let mut editor = TrainNicknameEditor::start(&state, train_id).unwrap();
        while !editor.draft().is_empty() {
            editor.handle_key(KeyCode::Backspace);
        }
        assert_eq!(
            editor.handle_key(KeyCode::Enter),
            TrainNicknameEditorAction::Confirm {
                train_id,
                nickname: None,
            }
        );
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
