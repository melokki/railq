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

mod assignment;
mod dashboard;

use assignment::{ServiceAssignmentAction, ServiceAssignmentFlow};

use crate::{
    catalog::{model_for_train, train_catalogue},
    model::{
        GameState, Journey, Money, RailStationId, ServiceDirectionMode, Train, TrainId,
        TrainNickname, TrainStatus, UtcSeconds,
    },
    ui::{
        components::{EmptyState, labelled_line, labelled_line_styled, panel_block, section_heading},
        layout::UiSize,
        modal, theme,
    },
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
    let card = modal::editor_rect(area, card_height);
    let footer = if card.width >= 56 {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Backspace", modal::ModalAction::Erase),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
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

/// Result of routing one Fleet-owned keyboard event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FleetWorkspaceAction {
    Continue,
    ClearNotice,
    Notice(String),
    SellTrain {
        train_id: TrainId,
    },
    UpdateNickname {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
    Dispatch {
        train_id: TrainId,
    },
    AssignService {
        train_id: TrainId,
        service_id: crate::model::ServiceId,
    },
    UnassignService {
        train_id: TrainId,
    },
}

/// One contextual footer action owned by the Fleet workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl FleetShortcut {
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

/// Owns presentation state and keyboard interaction for the Fleet workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FleetWorkspace {
    flow: Option<FleetFlow>,
    selection: FleetSelection,
    details_open: bool,
    split_visible: bool,
    nickname_editor: Option<TrainNicknameEditor>,
    assignment_flow: Option<ServiceAssignmentFlow>,
}

impl FleetWorkspace {
    pub fn activate(&mut self) {
        self.details_open = false;
        self.split_visible = false;
    }

    pub fn has_resale_flow(&self) -> bool {
        self.flow.is_some()
    }

    pub fn has_nickname_editor(&self) -> bool {
        self.nickname_editor.is_some()
    }

    pub fn has_assignment_flow(&self) -> bool {
        self.assignment_flow.is_some()
    }

    pub fn has_modal(&self) -> bool {
        self.has_resale_flow() || self.has_nickname_editor() || self.has_assignment_flow()
    }

    /// Returns contextual footer actions for the currently focused Fleet state.
    pub fn shortcuts(
        &mut self,
        state: &GameState,
        compact: bool,
        wide: bool,
    ) -> Vec<FleetShortcut> {
        if let Some(flow) = &self.assignment_flow {
            return flow
                .footer_shortcuts(state, compact)
                .into_iter()
                .map(|(key, action, enabled)| {
                    if enabled {
                        FleetShortcut::enabled(key, action)
                    } else {
                        FleetShortcut::disabled(key, action)
                    }
                })
                .collect();
        }

        if self.nickname_editor.is_some() {
            return vec![
                FleetShortcut::enabled("Enter", "Save"),
                FleetShortcut::enabled("Backspace", "Delete"),
                FleetShortcut::enabled("Esc", "Cancel"),
            ];
        }

        if self.flow.is_some() {
            return vec![
                FleetShortcut::enabled("Enter", "Resell"),
                FleetShortcut::enabled("Esc", "Cancel"),
            ];
        }

        if state.player_company.fleet.trains.is_empty() {
            return if has_delivery_station(state) {
                vec![FleetShortcut::enabled("3", "Market")]
            } else {
                vec![FleetShortcut::enabled("5", "Authority")]
            };
        }

        let mut items = if self.details_open {
            vec![FleetShortcut::enabled("Esc", "Back")]
        } else {
            let mut items = vec![FleetShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Train",
            )];
            if wide {
                items.push(FleetShortcut::enabled("PgUp/PgDn", "Page"));
            }
            if !self.split_visible {
                items.push(FleetShortcut::enabled("Enter", "Details"));
            }
            items
        };
        items.extend(self.action_shortcuts(state));
        items
    }

    /// Returns help content for the currently focused Fleet state.
    pub fn help_lines(&self, state: &GameState) -> Vec<String> {
        if self.assignment_flow.is_some() {
            return vec![
                "Current · Service Assignment".into(),
                "↑↓ / jk Select Passenger Service".into(),
                "Enter Assign or unassign   Esc Cancel".into(),
            ];
        }

        if self.nickname_editor.is_some() {
            return vec![
                "Current · Train Name".into(),
                "Type to edit the Train nickname".into(),
                "Backspace Delete the previous character".into(),
                "Enter Save nickname   Esc Cancel".into(),
            ];
        }

        if self.flow.is_some() {
            return vec![
                "Current · Train Resale".into(),
                "Enter Confirm resale".into(),
                "Esc Cancel".into(),
            ];
        }

        let mut lines = vec!["Current · Fleet".into()];
        if state.player_company.fleet.trains.is_empty() {
            lines.extend(["No trains owned yet".into(), String::new(), "Next step".into()]);
            lines.push(if has_delivery_station(state) {
                "3 Open Market and acquire your first passenger Train".into()
            } else {
                "5 Open Authority and wait for the first delivery Rail Station".into()
            });
        } else if self.details_open {
            lines.extend([
                "Esc Back to Fleet".into(),
                "r Rename selected Train".into(),
                "a Assign/change Passenger Service".into(),
                "u Unassign Passenger Service".into(),
                "d Dispatch or position selected READY Train".into(),
                "s Review resale of selected READY Train".into(),
            ]);
        } else {
            lines.extend([
                "↑↓ / jk Select Train".into(),
                "PgUp / PgDn Scroll".into(),
                "Enter Details".into(),
                "r Rename selected Train".into(),
                "a Assign/change Passenger Service".into(),
                "u Unassign Passenger Service".into(),
                "d Dispatch or position selected READY Train".into(),
                "s Review resale of selected READY Train".into(),
            ]);
        }
        lines
    }

    fn action_shortcuts(&mut self, state: &GameState) -> Vec<FleetShortcut> {
        let selected = self.selection.selected_train_id(state).and_then(|id| {
            state
                .player_company
                .fleet
                .trains
                .iter()
                .find(|train| train.id == id)
        });
        let has_selection = selected.is_some();
        let assigned_service_id = selected.and_then(|train| {
            state
                .player_company
                .fleet
                .assigned_service_id(train.id)
        });
        let ready_operation = selected.and_then(|train| match &train.status {
            TrainStatus::Ready { at } => Some(ready_train_operation(state, train.id, *at)),
            TrainStatus::Travelling { .. } => None,
        });

        vec![
            if has_selection {
                FleetShortcut::enabled("R", "Rename")
            } else {
                FleetShortcut::disabled("R", "Rename")
            },
            if ready_operation.is_some() {
                FleetShortcut::enabled(
                    "A",
                    if assigned_service_id.is_some() {
                        "Service"
                    } else {
                        "Assign"
                    },
                )
            } else {
                FleetShortcut::disabled("A", "Service")
            },
            if ready_operation.is_some() && assigned_service_id.is_some() {
                FleetShortcut::enabled("U", "Unassign")
            } else {
                FleetShortcut::disabled("U", "Unassign")
            },
            match ready_operation.and_then(ReadyTrainOperation::dispatch_action) {
                Some(action) => FleetShortcut::enabled("D", action),
                None => FleetShortcut::disabled("D", "Dispatch"),
            },
            if ready_operation.is_some() {
                FleetShortcut::enabled("S", "Sell")
            } else {
                FleetShortcut::disabled("S", "Sell")
            },
        ]
    }

    pub fn close_details(&mut self) {
        self.details_open = false;
    }

    pub fn handle_nickname_key(&mut self, key: KeyCode) -> FleetWorkspaceAction {
        let Some(editor) = &mut self.nickname_editor else {
            return FleetWorkspaceAction::Continue;
        };
        match editor.handle_key(key) {
            TrainNicknameEditorAction::Continue => FleetWorkspaceAction::Continue,
            TrainNicknameEditorAction::Cancel => {
                self.nickname_editor = None;
                FleetWorkspaceAction::Notice("Train rename cancelled; no changes were made.".into())
            }
            TrainNicknameEditorAction::Confirm { train_id, nickname } => {
                FleetWorkspaceAction::UpdateNickname { train_id, nickname }
            }
        }
    }

    pub fn handle_resale_key(&mut self, key: KeyEvent, state: &GameState) -> FleetWorkspaceAction {
        let Some(flow) = &mut self.flow else {
            return FleetWorkspaceAction::Continue;
        };
        match flow.handle_key(key, state) {
            FleetFlowAction::Continue => FleetWorkspaceAction::Continue,
            FleetFlowAction::Cancel => {
                self.flow = None;
                FleetWorkspaceAction::Notice("Train resale cancelled; no changes were made.".into())
            }
            FleetFlowAction::Confirm { train_id } => FleetWorkspaceAction::SellTrain { train_id },
        }
    }

    pub fn handle_assignment_key(
        &mut self,
        key: KeyCode,
        state: &GameState,
    ) -> FleetWorkspaceAction {
        let Some(flow) = &mut self.assignment_flow else {
            return FleetWorkspaceAction::Continue;
        };
        match flow.handle_key(key, state) {
            ServiceAssignmentAction::Continue => FleetWorkspaceAction::Continue,
            ServiceAssignmentAction::Cancel => {
                self.assignment_flow = None;
                FleetWorkspaceAction::Continue
            }
            ServiceAssignmentAction::Assign {
                train_id,
                service_id,
            } => FleetWorkspaceAction::AssignService {
                train_id,
                service_id,
            },
            ServiceAssignmentAction::Unassign { train_id } => {
                FleetWorkspaceAction::UnassignService { train_id }
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> FleetWorkspaceAction {
        match key.code {
            KeyCode::Enter if !self.split_visible => {
                if self.selection.selected_train_id(state).is_some() {
                    self.details_open = true;
                    FleetWorkspaceAction::ClearNotice
                } else {
                    FleetWorkspaceAction::Continue
                }
            }
            KeyCode::Esc if self.details_open => {
                self.details_open = false;
                FleetWorkspaceAction::Continue
            }
            KeyCode::Char('r' | 'R' | 'n' | 'N') => match self.selection.selected_train_id(state) {
                Some(train_id) => match TrainNicknameEditor::start(state, train_id) {
                    Ok(editor) => {
                        self.nickname_editor = Some(editor);
                        FleetWorkspaceAction::ClearNotice
                    }
                    Err(message) => FleetWorkspaceAction::Notice(message),
                },
                None => FleetWorkspaceAction::Notice("Select a Train before renaming it.".into()),
            },
            KeyCode::Char('a' | 'A') => match self.selection.selected_train_id(state) {
                Some(train_id) => match ServiceAssignmentFlow::start(state, train_id) {
                    Ok(flow) => {
                        self.assignment_flow = Some(flow);
                        FleetWorkspaceAction::ClearNotice
                    }
                    Err(message) => FleetWorkspaceAction::Notice(message),
                },
                None => FleetWorkspaceAction::Notice(
                    "Select a Train before assigning a Passenger Service.".into(),
                ),
            },
            KeyCode::Char('u' | 'U') => match self.selection.selected_train_id(state) {
                Some(train_id) => {
                    let Some(train) = state
                        .player_company
                        .fleet
                        .trains
                        .iter()
                        .find(|train| train.id == train_id)
                    else {
                        return FleetWorkspaceAction::Notice(
                            "Selected Train is no longer in the Fleet.".into(),
                        );
                    };
                    if matches!(&train.status, TrainStatus::Travelling { .. }) {
                        FleetWorkspaceAction::Notice(format!(
                            "Train {:02} is travelling; unassign it after arrival.",
                            train_id.get()
                        ))
                    } else if state
                        .player_company
                        .fleet
                        .assigned_service_id(train_id)
                        .is_none()
                    {
                        FleetWorkspaceAction::Notice(format!(
                            "Train {:02} is already unassigned.",
                            train_id.get()
                        ))
                    } else {
                        FleetWorkspaceAction::UnassignService { train_id }
                    }
                }
                None => FleetWorkspaceAction::Notice(
                    "Select a Train before removing its Passenger Service assignment.".into(),
                ),
            },
            KeyCode::Char('s' | 'S') => match self.selection.selected_train_id(state) {
                Some(train_id) => match FleetFlow::start(state, train_id) {
                    Ok(flow) => {
                        self.flow = Some(flow);
                        FleetWorkspaceAction::ClearNotice
                    }
                    Err(message) => FleetWorkspaceAction::Notice(message),
                },
                None => FleetWorkspaceAction::Notice(
                    "Select a Train before starting a resale review.".into(),
                ),
            },
            KeyCode::Char('d' | 'D') => match self.selection.selected_train_id(state) {
                Some(train_id) => FleetWorkspaceAction::Dispatch { train_id },
                None => FleetWorkspaceAction::Notice(
                    "Select a READY Train before starting Manual Dispatch.".into(),
                ),
            },
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                self.selection.handle_key(key.code, state);
                FleetWorkspaceAction::Continue
            }
            _ => FleetWorkspaceAction::Continue,
        }
    }

    pub fn reject_resale(&mut self, error: impl Into<String>) -> Option<String> {
        let error = error.into();
        if let Some(flow) = &mut self.flow {
            flow.reject(error);
            None
        } else {
            Some(error)
        }
    }

    pub fn confirm_resale_saved(&mut self) {
        self.flow = None;
        self.details_open = false;
    }

    pub fn confirm_nickname_saved(&mut self) -> Option<TrainId> {
        let train_id = self
            .nickname_editor
            .as_ref()
            .map(TrainNicknameEditor::train_id);
        self.nickname_editor = None;
        train_id
    }

    pub fn reject_assignment(&mut self, error: impl Into<String>) -> Option<String> {
        let error = error.into();
        if let Some(flow) = &mut self.assignment_flow {
            flow.reject(error);
            None
        } else {
            Some(error)
        }
    }

    pub fn confirm_assignment_saved(&mut self) {
        self.assignment_flow = None;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn render_dashboard(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        state: &GameState,
        now: UtcSeconds,
    ) {
        self.split_visible = UiSize::from_rect(area).supports_split_view();
        if self.split_visible {
            self.details_open = false;
        }
        render_dashboard(
            frame,
            area,
            state,
            now,
            &mut self.selection,
            self.details_open,
        );
    }

    pub fn render_modal(&self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(flow) = &self.assignment_flow {
            assignment::render(frame, modal::workflow_rect(area), state, flow);
        } else if let Some(editor) = &self.nickname_editor {
            render_nickname_editor(frame, area, editor, state);
        } else if let Some(flow) = &self.flow {
            flow.render_review(frame, area, state);
        }
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
    let card = modal::confirmation_rect(area, 16);
    let footer = if review.is_ok() {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Resell),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    } else {
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )])
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
        render_empty_fleet(frame, area, state);
        return;
    }

    if UiSize::from_rect(area).supports_split_view() {
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

    let [overview_area, metrics_area, fleet_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(5),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(shell_inner);
    dashboard::render_overview(frame, overview_area, state);
    dashboard::render_metrics(frame, metrics_area, state, now);

    let [list_area, inspector_area] =
        Layout::horizontal([Constraint::Min(48), Constraint::Length(48)]).areas(fleet_area);
    let list_area = horizontal_inset(list_area, 1);
    let [list_heading_area, list_table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(list_area);
    frame.render_widget(
        Paragraph::new(section_heading("ROLLING STOCK")).style(theme::panel()),
        list_heading_area,
    );
    let visible_items = usize::from(list_table_area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);

    let show_service = list_table_area.width >= 72;
    let rows = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| {
            let fields = train_fields(state, train, now);
            if show_service {
                Row::new(vec![
                    Cell::from(train_picker_label(train, &fields.model)),
                    Cell::from(fields.status).style(train_status_style(train)),
                    Cell::from(
                        assigned_service_label(state, train.id).unwrap_or_else(|| "—".into()),
                    )
                    .style(theme::secondary()),
                    Cell::from(fields.place),
                    Cell::from(fields.eta),
                ])
            } else {
                Row::new(vec![
                    Cell::from(train_picker_label(train, &fields.model)),
                    Cell::from(fields.status).style(train_status_style(train)),
                    Cell::from(fields.place),
                    Cell::from(fields.eta),
                ])
            }
        })
        .collect::<Vec<_>>();
    let (header, widths) = if show_service {
        (
            Row::new(["Train", "State", "Service", "Position", "ETA"])
                .style(theme::table_header())
                .bottom_margin(1),
            vec![
                Constraint::Min(18),
                Constraint::Length(11),
                Constraint::Length(18),
                Constraint::Min(10),
                Constraint::Length(9),
            ],
        )
    } else {
        (
            Row::new(["Train", "State", "Position", "ETA"])
                .style(theme::table_header())
                .bottom_margin(1),
            vec![
                Constraint::Min(16),
                Constraint::Length(11),
                Constraint::Min(10),
                Constraint::Length(9),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(header)
        .style(theme::panel())
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(FLEET_SELECTION_MARKER)
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, list_table_area, &mut selection.table_state);

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

fn render_empty_fleet(frame: &mut Frame, area: Rect, state: &GameState) {
    let block = panel_block("Fleet", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let empty_state = if !has_delivery_station(state) {
        EmptyState::blocked(
            "No rolling stock",
            "Your company does not own any trains yet.",
            "The Authority must open a Rail Station before rolling stock can be delivered.",
        )
    } else if catalogue_train_is_affordable(state) {
        EmptyState::first_use(
            "No rolling stock",
            "Your company does not own any trains yet.",
            "3",
            "Open Market",
        )
        .hint("Purchase your first passenger Train and choose its delivery Rail Station.")
    } else {
        EmptyState::first_use(
            "No rolling stock",
            "Your company does not own any trains yet.",
            "3",
            "Open Market",
        )
        .hint("No catalogue Train is currently affordable with available Company cash.")
    }
    .motif("╾━╼");

    empty_state.render(frame, inner);
}

fn has_delivery_station(state: &GameState) -> bool {
    !state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .is_empty()
}

fn catalogue_train_is_affordable(state: &GameState) -> bool {
    train_catalogue()
        .models()
        .iter()
        .any(|train| state.player_company.funds >= train.purchase_price())
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
    // A short-but-wide terminal still needs the operational facts to fit. In
    // that case use the same dense hierarchy as the compact detail page.
    let dense_detail = compact_detail || details_area.height < 30;

    let mut lines = Vec::new();
    if embedded {
        lines.push(section_heading("SELECTED TRAIN"));
        lines.push(Line::styled(title.clone(), theme::focused_title()));
        lines.push(Line::styled(fields.model.clone(), theme::primary_value()));
        lines.push(selected_train_summary(state, train, journey));
    } else {
        lines.push(Line::from(vec![
            Span::styled(fields.model.clone(), theme::primary_value()),
            Span::styled(" · ", theme::secondary()),
            Span::styled(train.evn.formatted(), theme::secondary()),
        ]));
        lines.push(selected_train_summary(state, train, journey));
    }

    match (&train.status, journey) {
        (TrainStatus::Ready { at }, _) => {
            let operation = ready_train_operation(state, train.id, *at);
            let (availability, availability_style) = operation.availability();
            let (next_action, next_action_style) = operation.next_action(state, train.id);
            inspector_section(&mut lines, "OPERATIONS", dense_detail);
            lines.push(labelled_line_styled(
                "State",
                train_status_label(train),
                train_status_style(train),
            ));
            lines.push(labelled_line_styled(
                "Availability",
                availability,
                availability_style,
            ));
            lines.push(labelled_line(
                "Station",
                &station_label_or_missing(state, *at),
            ));
            lines.push(labelled_line_styled(
                "Next action",
                &next_action,
                next_action_style,
            ));

            inspector_section(&mut lines, "ASSIGNMENT", dense_detail);
            lines.push(labelled_line(
                "Service",
                &assigned_service_label(state, train.id).unwrap_or_else(|| "Unassigned".into()),
            ));
            lines.push(labelled_line(
                "Route",
                &assigned_service_route_label(state, train.id).unwrap_or_else(|| "—".into()),
            ));

            inspector_section(&mut lines, "CAPABILITY", dense_detail);
            lines.push(labelled_line("Seats", &format_capacity(train)));
            if dense_detail {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<18}", "Technical"), theme::secondary()),
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

            inspector_section(&mut lines, "IDENTITY", dense_detail);
            lines.push(labelled_line("EVN", &train.evn.formatted()));
            lines.push(labelled_line(
                "Keeper mark",
                &format!(
                    "{}-{}",
                    state.region.railway_registration.mark,
                    state.player_company.vehicle_keeper_mark.as_str(),
                ),
            ));

            inspector_section(&mut lines, "ASSET VALUE", dense_detail);
            if !dense_detail {
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
        (TrainStatus::Travelling { .. }, Some(journey)) => {
            inspector_section(&mut lines, "JOURNEY", dense_detail);
            lines.push(labelled_line_styled(
                "State",
                train_status_label(train),
                train_status_style(train),
            ));
            lines.push(labelled_line(
                "Current leg",
                &journey_leg_label(state, journey),
            ));
            if let Some(next_stop) = journey_next_stop_station_id(state, journey) {
                lines.push(labelled_line(
                    "Next stop",
                    &station_label_or_missing(state, next_stop),
                ));
            }
            if !dense_detail {
                lines.push(labelled_line(
                    "Departed",
                    &format!("{} ago", format_duration(elapsed_seconds(journey, now))),
                ));
            }
            lines.push(labelled_line(
                "ETA",
                &format!("in {}", format_duration(remaining_seconds(journey, now))),
            ));
            lines.push(labelled_line(
                "Leg progress",
                &format!(
                    "{}% · {} elapsed",
                    journey_progress_percent(journey, now),
                    format_duration(elapsed_seconds(journey, now)),
                ),
            ));

            inspector_section(&mut lines, "SERVICE", dense_detail);
            if let Some(service) = state
                .player_company
                .passenger_services
                .iter()
                .find(|service| service.id == journey.service_id)
            {
                lines.push(labelled_line("Service", &service.display_name()));
                if !dense_detail {
                    lines.push(labelled_line(
                        "Route",
                        &format!(
                            "{} → {}",
                            station_label_or_missing(state, journey.origin_station_id),
                            station_label_or_missing(state, journey.destination_station_id),
                        ),
                    ));
                }
            } else {
                lines.push(labelled_line(
                    "Service",
                    &format!("Missing service {}", journey.service_id.get()),
                ));
            }

            inspector_section(&mut lines, "PASSENGERS", dense_detail);
            let onboard = journey.onboard_passengers();
            lines.push(labelled_line(
                "On board",
                &format!("{} / {}", onboard, train_capacity(train)),
            ));
            lines.push(labelled_line("Load", &format_load(train, onboard)));
            if !dense_detail {
                lines.push(labelled_line(
                    "Carried",
                    &format!("{} total", journey.passengers_carried),
                ));
            }

            inspector_section(&mut lines, "COMMERCIAL", dense_detail);
            if dense_detail {
                match journey_expected_result(journey) {
                    Some(result) => lines.push(labelled_line_styled(
                        "Expected result",
                        &format_signed_money(result),
                        journey_result_style(result),
                    )),
                    None => lines.push(labelled_line("Expected result", "Unavailable")),
                }
            } else {
                lines.push(labelled_line(
                    "Expected revenue",
                    &format_money(journey.operating_revenue),
                ));
                lines.push(labelled_line(
                    "Credited",
                    &format_money(journey.credited_revenue),
                ));
                lines.push(labelled_line(
                    "Access fee",
                    &format_money(journey.infrastructure_access_fee),
                ));
                lines.push(labelled_line("Fuel cost", &format_money(journey.fuel_cost)));
                match journey_operating_cost(journey) {
                    Some(cost) => lines.push(labelled_line("Operating cost", &format_money(cost))),
                    None => lines.push(labelled_line("Operating cost", "Unavailable")),
                }
                match journey_expected_result(journey) {
                    Some(result) => lines.push(labelled_line_styled(
                        "Expected result",
                        &format_signed_money(result),
                        journey_result_style(result),
                    )),
                    None => lines.push(labelled_line("Expected result", "Unavailable")),
                }
            }

            inspector_section(&mut lines, "CAPABILITY", dense_detail);
            if dense_detail {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<18}", "Technical"), theme::secondary()),
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

            inspector_section(&mut lines, "IDENTITY", dense_detail);
            lines.push(labelled_line("EVN", &train.evn.formatted()));
            lines.push(labelled_line(
                "Keeper mark",
                &format!(
                    "{}-{}",
                    state.region.railway_registration.mark,
                    state.player_company.vehicle_keeper_mark.as_str(),
                ),
            ));
        }
        (TrainStatus::Travelling { journey_id }, None) => {
            inspector_section(&mut lines, "OPERATIONS", dense_detail);
            lines.push(labelled_line_styled(
                "State",
                train_status_label(train),
                train_status_style(train),
            ));
            lines.push(labelled_line("Availability", "Journey data unavailable"));

            inspector_section(&mut lines, "JOURNEY", dense_detail);
            lines.push(labelled_line("Journey", &journey_id.get().to_string()));
            lines.push(labelled_line("Details", "Unavailable"));

            inspector_section(&mut lines, "CAPABILITY", dense_detail);
            if dense_detail {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<18}", "Technical"), theme::secondary()),
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

            inspector_section(&mut lines, "IDENTITY", dense_detail);
            lines.push(labelled_line("EVN", &train.evn.formatted()));
            lines.push(labelled_line(
                "Keeper mark",
                &format!(
                    "{}-{}",
                    state.region.railway_registration.mark,
                    state.player_company.vehicle_keeper_mark.as_str(),
                ),
            ));
        }
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
                .label(journey_progress_label(state, journey, percent))
                .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
                .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
            progress_area,
        );
    }
}

fn selected_train_summary(
    state: &GameState,
    train: &Train,
    journey: Option<&Journey>,
) -> Line<'static> {
    let place = match (&train.status, journey) {
        (TrainStatus::Ready { at }, _) => station_label_or_missing(state, *at),
        (TrainStatus::Travelling { .. }, Some(journey)) => journey_leg_label(state, journey),
        (TrainStatus::Travelling { .. }, None) => "Journey details unavailable".into(),
    };
    Line::from(vec![
        Span::styled(
            train_status_label(train),
            train_status_style(train).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", theme::secondary()),
        Span::styled(place, theme::secondary()),
    ])
}

fn assigned_service_route_label(state: &GameState, train_id: TrainId) -> Option<String> {
    let service_id = state.player_company.fleet.assigned_service_id(train_id)?;
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)?;
    let origin = service.origin_station_id()?;
    let destination = service.destination_station_id()?;
    let separator = if service.direction_mode == ServiceDirectionMode::BothDirections {
        " ↔ "
    } else {
        " → "
    };
    Some(format!(
        "{}{}{}",
        station_label_or_missing(state, origin),
        separator,
        station_label_or_missing(state, destination),
    ))
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

fn journey_progress_label(state: &GameState, journey: &Journey, percent: u64) -> String {
    journey_next_stop_station_id(state, journey)
        .map(|station_id| {
            format!(
                "{percent}% · {}",
                station_label_or_missing(state, station_id),
            )
        })
        .unwrap_or_else(|| format!("Leg {percent}%"))
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
    let service = state
        .player_company
        .fleet
        .assigned_service_id(train.id)
        .map(|service_id| format!(" · R{}", service_id.get()))
        .unwrap_or_default();
    [
        Line::styled(
            format!(
                "{marker} Train {:02}{nickname}  {}{service}",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyTrainOperation {
    AssignmentRequired,
    ServiceUnavailable,
    ReadyToDispatch,
    PositioningRequired,
}

impl ReadyTrainOperation {
    fn availability(self) -> (&'static str, Style) {
        match self {
            Self::AssignmentRequired => ("Assignment required", theme::warning()),
            Self::ServiceUnavailable => ("Assigned Service unavailable", theme::error()),
            Self::ReadyToDispatch => ("Ready to dispatch", theme::success()),
            Self::PositioningRequired => ("Positioning required", theme::warning()),
        }
    }

    fn dispatch_action(self) -> Option<&'static str> {
        match self {
            Self::ReadyToDispatch => Some("Dispatch"),
            Self::PositioningRequired => Some("Position"),
            Self::AssignmentRequired | Self::ServiceUnavailable => None,
        }
    }

    fn next_action(self, state: &GameState, train_id: TrainId) -> (String, Style) {
        match self {
            Self::AssignmentRequired => ("Assign Passenger Service".into(), theme::warning()),
            Self::ServiceUnavailable => ("Review Service assignment".into(), theme::error()),
            Self::ReadyToDispatch => (
                assigned_service_label(state, train_id)
                    .map(|service| format!("Dispatch {service}"))
                    .unwrap_or_else(|| "Dispatch assigned Service".into()),
                theme::focused_title(),
            ),
            Self::PositioningRequired => (
                assigned_service_label(state, train_id)
                    .map(|service| format!("Position for {service}"))
                    .unwrap_or_else(|| "Position for assigned Service".into()),
                theme::warning(),
            ),
        }
    }
}

fn ready_train_operation(
    state: &GameState,
    train_id: TrainId,
    station_id: RailStationId,
) -> ReadyTrainOperation {
    let Some(service_id) = state.player_company.fleet.assigned_service_id(train_id) else {
        return ReadyTrainOperation::AssignmentRequired;
    };
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return ReadyTrainOperation::ServiceUnavailable;
    };

    let can_depart = service.origin_station_id() == Some(station_id)
        || (service.direction_mode == ServiceDirectionMode::BothDirections
            && service.destination_station_id() == Some(station_id));
    if can_depart {
        ReadyTrainOperation::ReadyToDispatch
    } else {
        ReadyTrainOperation::PositioningRequired
    }
}

fn assigned_service_label(state: &GameState, train_id: TrainId) -> Option<String> {
    let service_id = state.player_company.fleet.assigned_service_id(train_id)?;
    Some(
        state
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .map(|service| service.display_name())
            .unwrap_or_else(|| format!("R{} · Missing service", service_id.get())),
    )
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
                place: journey_leg_label(state, journey),
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

fn train_capacity(train: &Train) -> String {
    model_for_train(train)
        .map(|model| model.passenger_capacity().passengers().to_string())
        .unwrap_or_else(|| "?".into())
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
    let cents =
        i64::try_from(model.fuel_cost_per_kilometre().cents_per_kilometre()).unwrap_or(i64::MAX);
    format!("{}/km", format_money(Money::from_cents(cents)))
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
        writeln!(output, "\nNo Trains yet. Next useful action: 3 · Market.")
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
    if state
        .player_company
        .fleet
        .trains
        .iter()
        .all(|train| matches!(train.status, TrainStatus::Travelling { .. }))
    {
        if let Some(eta) = nearest_arrival(state, now) {
            writeln!(output, "Next arrival: {eta}.").expect("writing to a String cannot fail");
        }
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
    let direction = if journey.origin_station_id == first && journey.destination_station_id == last
    {
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
    let duration = leg_duration_seconds(journey);
    if duration == 0 {
        return 100;
    }
    elapsed_seconds(journey, now).saturating_mul(100) / duration
}

fn leg_duration_seconds(journey: &Journey) -> u64 {
    u64::try_from(
        journey
            .arrives_at
            .unix_seconds()
            .saturating_sub(journey.departed_at.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

fn elapsed_seconds(journey: &Journey, now: UtcSeconds) -> u64 {
    let duration = leg_duration_seconds(journey);
    u64::try_from(
        now.unix_seconds()
            .saturating_sub(journey.departed_at.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
    .min(duration)
}

fn journey_operating_cost(journey: &Journey) -> Option<Money> {
    journey
        .infrastructure_access_fee
        .checked_add(journey.fuel_cost)
        .ok()
}

fn journey_expected_result(journey: &Journey) -> Option<Money> {
    journey_operating_cost(journey)
        .and_then(|cost| journey.operating_revenue.checked_sub(cost).ok())
}

fn format_signed_money(money: Money) -> String {
    crate::ui::format::signed_cents(i128::from(money.cents()))
}

fn journey_result_style(result: Money) -> Style {
    if result.cents() > 0 {
        theme::success()
    } else if result.cents() < 0 {
        theme::warning()
    } else {
        theme::secondary()
    }
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
            services::{create_service, find_or_create_service},
            world::create_new_game,
        },
    };

    use super::{
        FleetFlow, FleetFlowAction, FleetWorkspace, FleetWorkspaceAction, TrainNicknameEditor,
        TrainNicknameEditorAction, render_at,
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
    fn fleet_workspace_owns_resale_interaction_state() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut workspace = FleetWorkspace::default();

        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('s')), &state),
            FleetWorkspaceAction::ClearNotice
        );
        assert!(workspace.has_resale_flow());
        assert_eq!(
            workspace.handle_resale_key(key(KeyCode::Enter), &state),
            FleetWorkspaceAction::SellTrain { train_id }
        );
    }

    #[test]
    fn fleet_workspace_can_open_service_assignment_for_the_selected_train() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let mut workspace = FleetWorkspace::default();

        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('a')), &state),
            FleetWorkspaceAction::ClearNotice
        );
        assert!(workspace.has_assignment_flow());
    }

    #[test]
    fn fleet_workspace_can_unassign_the_selected_ready_train_directly() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        crate::sim::services::assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let mut workspace = FleetWorkspace::default();

        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('u')), &state),
            FleetWorkspaceAction::UnassignService { train_id }
        );
    }

    #[test]
    fn fleet_dispatch_shortcut_requires_an_assignment_but_labels_positioning() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(2), RailStationId::new(3)],
        )
        .unwrap();
        let mut workspace = FleetWorkspace::default();

        let unassigned = workspace.shortcuts(&state, false, true);
        assert!(unassigned.iter().any(|shortcut| {
            shortcut.key == "A" && shortcut.action == "Assign" && shortcut.enabled
        }));
        assert!(unassigned.iter().any(|shortcut| {
            shortcut.key == "D" && shortcut.action == "Dispatch" && !shortcut.enabled
        }));

        crate::sim::services::assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let assigned = workspace.shortcuts(&state, false, true);
        assert!(assigned.iter().any(|shortcut| {
            shortcut.key == "D" && shortcut.action == "Position" && shortcut.enabled
        }));
        assert!(
            assigned
                .iter()
                .any(|shortcut| shortcut.key == "U" && shortcut.enabled)
        );
    }

    #[test]
    fn fleet_workspace_owns_contextual_controls_and_help() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut workspace = FleetWorkspace::default();

        let shortcuts = workspace.shortcuts(&state, false, true);
        assert!(shortcuts.iter().any(|shortcut| shortcut.action == "Rename"));
        assert!(
            shortcuts
                .iter()
                .any(|shortcut| shortcut.action == "Dispatch")
        );
        assert!(shortcuts.iter().any(|shortcut| shortcut.action == "Sell"));
        assert!(
            workspace
                .help_lines(&state)
                .iter()
                .any(|line| line == "Current · Fleet")
        );

        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('s')), &state),
            FleetWorkspaceAction::ClearNotice
        );
        assert_eq!(
            workspace
                .shortcuts(&state, false, true)
                .iter()
                .map(|shortcut| shortcut.action.as_str())
                .collect::<Vec<_>>(),
            vec!["Resell", "Cancel"]
        );
        assert_eq!(workspace.help_lines(&state)[0], "Current · Train Resale");
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
