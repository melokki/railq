//! Financial presentation for the Player Company's Company view.
//!
//! This module reads the stored financial totals and the simulation's finite
//! recovery evaluation. It does not mutate the game state or authorise any
//! recovery action.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Cell, HighlightSpacing, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
};

use crate::{
    catalog::train_catalogue,
    model::{GameState, JourneyId, JourneyReceipt, Money, RailStationId, VehicleKeeperMark},
    sim::finance::{
        FinancialEvaluation, FinancialStatus, RecoveryJourney, RecoveryOption,
        evaluate_financial_recovery,
    },
    ui::{components, modal, theme},
};

const MAXIMUM_RECOVERY_OPTIONS_SHOWN: usize = 3;

/// Presentation-only editor for the Player Company's Vehicle Keeper Mark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VkmEditor {
    draft: String,
    error: Option<String>,
}

/// Outcome of one key handled by the VKM editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VkmEditorAction {
    Continue,
    Cancel,
    Confirm(VehicleKeeperMark),
}

impl VkmEditor {
    /// Starts editing from the Company's currently persisted mark.
    pub fn start(state: &GameState) -> Self {
        Self {
            draft: state.player_company.vehicle_keeper_mark.as_str().to_owned(),
            error: None,
        }
    }

    /// Handles text editing without mutating the simulation state.
    pub fn handle_key(&mut self, key: KeyCode) -> VkmEditorAction {
        match key {
            KeyCode::Esc => VkmEditorAction::Cancel,
            KeyCode::Enter => match VehicleKeeperMark::parse(&self.draft) {
                Ok(mark) => VkmEditorAction::Confirm(mark),
                Err(error) => {
                    self.error = Some(error.to_string());
                    VkmEditorAction::Continue
                }
            },
            KeyCode::Backspace => {
                self.draft.pop();
                self.error = None;
                VkmEditorAction::Continue
            }
            KeyCode::Char(character) if character.is_ascii_alphabetic() && self.draft.len() < 5 => {
                self.draft.push(character.to_ascii_uppercase());
                self.error = None;
                VkmEditorAction::Continue
            }
            KeyCode::Char(_) => {
                self.error = Some("VKM accepts 2–5 letters A-Z only.".into());
                VkmEditorAction::Continue
            }
            _ => VkmEditorAction::Continue,
        }
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Renders the VKM editor using the shared focused-modal treatment.
pub fn render_vkm_editor(frame: &mut Frame, area: Rect, editor: &VkmEditor, state: &GameState) {
    let card = modal::editor_rect(area, 13);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "Edit Vehicle Keeper Mark",
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Backspace", modal::ModalAction::Erase),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
    );

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Company       ", theme::secondary()),
            Span::styled(state.player_company.name.clone(), theme::title()),
        ]),
        Line::from(""),
        section_heading("VEHICLE KEEPER MARK"),
        Line::from(vec![
            Span::styled("VKM           ", theme::secondary()),
            Span::styled(editor.draft().to_owned(), theme::focused_title()),
            Span::styled("▏", theme::focused_title()),
        ]),
        Line::from(""),
        Line::styled(
            "2–5 letters A–Z. The keeper mark changes; the numeric EVN does not.",
            theme::secondary(),
        ),
    ];
    if let Some(error) = editor.error() {
        lines.push(Line::from(""));
        lines.push(Line::styled(error.to_owned(), theme::error()));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

/// A workspace that can present the next review in a financial recovery route.
/// Opening one never performs the suggested action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDestination {
    /// The Fleet workspace, where Train resale remains separately reviewed.
    Fleet,
    /// The Buy Trains workspace, where a replacement remains separately reviewed.
    BuyTrains,
    /// The Map workspace, where Manual Dispatch remains separately reviewed.
    Map,
}

/// Presentation-only selection for the finite recovery routes calculated by
/// the simulation. The index deliberately has no simulation meaning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoverySelection {
    list_state: ListState,
    page_size: usize,
}

impl RecoverySelection {
    /// Moves between calculated recovery routes without modifying the Player Company.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        let Some(evaluation) = recovery_evaluation(state) else {
            return;
        };
        self.synchronize(evaluation.recovery_options.len());
        let Some(selected) = self.list_state.selected() else {
            return;
        };
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(evaluation.recovery_options.len().saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(self.page_size.max(1)),
            KeyCode::PageDown => selected
                .saturating_add(self.page_size.max(1))
                .min(evaluation.recovery_options.len().saturating_sub(1)),
            _ => selected,
        };
        self.list_state.select(Some(next));
    }

    /// Returns the workspace for the selected route's first deliberate review.
    pub fn selected_destination(&mut self, state: &GameState) -> Option<RecoveryDestination> {
        let evaluation = recovery_evaluation(state)?;
        self.synchronize(evaluation.recovery_options.len());
        let option = evaluation
            .recovery_options
            .get(self.list_state.selected()?)?;
        Some(match option {
            RecoveryOption::CashOnly { .. } => RecoveryDestination::Map,
            RecoveryOption::SellOthersAndRetain { .. } | RecoveryOption::SellAllAndRebuy { .. } => {
                RecoveryDestination::Fleet
            }
        })
    }

    fn synchronize(&mut self, option_count: usize) {
        let selected = if option_count == 0 {
            None
        } else {
            Some(
                self.list_state
                    .selected()
                    .unwrap_or(0)
                    .min(option_count.saturating_sub(1)),
            )
        };
        if selected.is_none() {
            *self.list_state.offset_mut() = 0;
        }
        self.list_state.select(selected);
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
}

/// Persistent receipt browsing state. A receipt is selected by Journey ID, not
/// table position, so a newly settled Journey cannot move the reader to a
/// different receipt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReceiptSelection {
    selected_journey_id: Option<JourneyId>,
    table_state: TableState,
    page_size: usize,
}

impl ReceiptSelection {
    /// Moves the retained-receipt selection without changing the saved game.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let receipt_count = state.financials.recent_journey_receipts.len();
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(receipt_count.saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(receipt_count.saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    /// Returns whether a receipt can be inspected after reconciling arrivals.
    pub fn has_selection(&mut self, state: &GameState) -> bool {
        self.synchronize(state);
        self.selected_journey_id.is_some()
    }

    fn synchronize(&mut self, state: &GameState) {
        let receipts = &state.financials.recent_journey_receipts;
        let previous_index = self.table_state.selected().unwrap_or(0);
        let selected = self
            .selected_journey_id
            .and_then(|journey_id| {
                receipts
                    .iter()
                    .rev()
                    .position(|receipt| receipt.journey_id == journey_id)
            })
            .or_else(|| {
                (!receipts.is_empty())
                    .then_some(previous_index.min(receipts.len().saturating_sub(1)))
            });
        self.selected_journey_id = selected.and_then(|index| {
            receipts
                .iter()
                .rev()
                .nth(index)
                .map(|receipt| receipt.journey_id)
        });
        if selected.is_none() {
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(receipt) = state
            .financials
            .recent_journey_receipts
            .iter()
            .rev()
            .nth(index)
        else {
            return;
        };
        self.selected_journey_id = Some(receipt.journey_id);
        self.table_state.select(Some(index));
    }

    fn selected_receipt<'a>(&mut self, state: &'a GameState) -> Option<&'a JourneyReceipt> {
        self.synchronize(state);
        self.selected_journey_id.and_then(|journey_id| {
            state
                .financials
                .recent_journey_receipts
                .iter()
                .find(|receipt| receipt.journey_id == journey_id)
        })
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
}

/// Shell-facing outcome from the Company workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompanyWorkspaceAction {
    /// No application action is required.
    Continue,
    /// Clear stale Shell feedback after a Company-only transition.
    ClearNotice,
    /// Surface presentation feedback without crossing the application boundary.
    Notice(String),
    /// Open another workspace while reviewing a financial recovery route.
    Navigate(RecoveryDestination),
    /// Revalidate and persist the edited Vehicle Keeper Mark.
    UpdateVkm(VehicleKeeperMark),
}

/// One contextual footer action owned by the Company workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompanyShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl CompanyShortcut {
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

/// Owns presentation state and keyboard interaction for the Company workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompanyWorkspace {
    receipt_selection: ReceiptSelection,
    receipt_details_open: bool,
    recovery_selection: RecoverySelection,
    recovery_review_open: bool,
    vkm_editor: Option<VkmEditor>,
}

impl CompanyWorkspace {
    /// Clears transient Company workflows when the primary view is reopened.
    pub fn activate(&mut self) {
        self.receipt_details_open = false;
        self.recovery_review_open = false;
        self.vkm_editor = None;
    }

    /// Clears all transient Company presentation state after a fresh-game restart.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns whether any Company-owned focused workflow is visible.
    pub fn has_modal(&self) -> bool {
        self.vkm_editor.is_some() || self.recovery_review_open || self.receipt_details_open
    }

    /// Returns whether the VKM editor must receive text input before global shortcuts.
    pub fn has_vkm_editor(&self) -> bool {
        self.vkm_editor.is_some()
    }

    /// Returns whether recovery navigation must be handled before global workspace shortcuts.
    pub fn recovery_review_open(&self) -> bool {
        self.recovery_review_open
    }

    /// Returns contextual footer actions for the currently focused Company state.
    pub fn shortcuts(
        &mut self,
        state: &GameState,
        compact: bool,
        wide: bool,
    ) -> Vec<CompanyShortcut> {
        if self.vkm_editor.is_some() {
            return vec![
                CompanyShortcut::enabled("Enter", "Save"),
                CompanyShortcut::enabled("Backspace", "Delete"),
                CompanyShortcut::enabled("Esc", "Cancel"),
            ];
        }

        if self.recovery_review_open {
            let mut items = vec![CompanyShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Route",
            )];
            if wide {
                items.push(CompanyShortcut::enabled("PgUp/PgDn", "Page"));
            }
            items.push(CompanyShortcut::enabled("Enter", "Review"));
            items.push(CompanyShortcut::enabled("Esc", "Back"));
            return items;
        }

        if self.receipt_details_open {
            return vec![CompanyShortcut::enabled("Esc", "Back")];
        }

        let mut items = Vec::new();
        if !state.financials.recent_journey_receipts.is_empty() {
            items.push(CompanyShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Receipt",
            ));
            if wide {
                items.push(CompanyShortcut::enabled("PgUp/PgDn", "Page"));
            }
            items.push(CompanyShortcut::enabled("Enter", "Inspect"));
        }
        items.push(CompanyShortcut::enabled("V", "Edit VKM"));

        let recovery_available =
            evaluate_financial_recovery(state)
                .ok()
                .is_some_and(|evaluation| {
                    evaluation.status == FinancialStatus::Insolvent
                        && !evaluation.recovery_options.is_empty()
                });
        items.push(if recovery_available {
            CompanyShortcut::enabled("R", "Recovery")
        } else {
            CompanyShortcut::disabled("R", "Recovery")
        });
        items
    }

    /// Returns help content for the currently focused Company state.
    pub fn help_lines(&self, state: &GameState) -> Vec<String> {
        if self.vkm_editor.is_some() {
            return vec![
                "Current · Company VKM".into(),
                "Type A-Z to edit the Vehicle Keeper Mark".into(),
                "Backspace Delete the previous letter".into(),
                "Enter Save VKM   Esc Cancel".into(),
            ];
        }

        if self.recovery_review_open {
            return vec![
                "Current · Financial Recovery".into(),
                "↑↓ / jk Select a recovery route".into(),
                "Enter Open the selected recovery action".into(),
                "Esc Back to Company".into(),
            ];
        }

        if self.receipt_details_open {
            return vec![
                "Current · Journey Receipt".into(),
                "Esc Back to Journey history".into(),
                "1–6 Switch workspace".into(),
            ];
        }

        let mut lines = vec!["Current · Company".into(), "v Edit Company VKM".into()];
        if state.financials.recent_journey_receipts.is_empty() {
            lines.extend([
                "No settled Journey receipts yet".into(),
                "1 Return to Map to operate your railway".into(),
            ]);
        } else {
            lines.extend([
                "↑↓ / jk Select Journey receipt".into(),
                "PgUp / PgDn Scroll history".into(),
                "Enter Details".into(),
            ]);
        }
        if let Ok(evaluation) = evaluate_financial_recovery(state) {
            if evaluation.status != FinancialStatus::Operating {
                lines.push("r Review available financial recovery routes".into());
            }
        }
        lines
    }

    /// Routes one Company-owned keyboard event. Global primary-view navigation remains a Shell concern.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> CompanyWorkspaceAction {
        if let Some(editor) = &mut self.vkm_editor {
            return match editor.handle_key(key.code) {
                VkmEditorAction::Continue => CompanyWorkspaceAction::Continue,
                VkmEditorAction::Cancel => {
                    self.vkm_editor = None;
                    CompanyWorkspaceAction::Notice(
                        "VKM edit cancelled; no changes were made.".into(),
                    )
                }
                VkmEditorAction::Confirm(vehicle_keeper_mark) => {
                    CompanyWorkspaceAction::UpdateVkm(vehicle_keeper_mark)
                }
            };
        }

        if self.recovery_review_open {
            return match key.code {
                KeyCode::Esc => {
                    self.recovery_review_open = false;
                    CompanyWorkspaceAction::Notice(
                        "Recovery review closed; no changes were made.".into(),
                    )
                }
                KeyCode::Enter => {
                    let Some(destination) = self.recovery_selection.selected_destination(state)
                    else {
                        self.recovery_review_open = false;
                        return CompanyWorkspaceAction::Notice(
                            "Recovery route changed; review the current Company status again."
                                .into(),
                        );
                    };
                    self.recovery_review_open = false;
                    CompanyWorkspaceAction::Navigate(destination)
                }
                KeyCode::Char('1') => {
                    self.recovery_review_open = false;
                    CompanyWorkspaceAction::Navigate(RecoveryDestination::Map)
                }
                KeyCode::Char('2' | 't' | 'T') => {
                    self.recovery_review_open = false;
                    CompanyWorkspaceAction::Navigate(RecoveryDestination::Fleet)
                }
                KeyCode::Char('3' | 'b' | 'B') => {
                    self.recovery_review_open = false;
                    CompanyWorkspaceAction::Navigate(RecoveryDestination::BuyTrains)
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                    self.recovery_selection.handle_key(key.code, state);
                    CompanyWorkspaceAction::Continue
                }
                _ => CompanyWorkspaceAction::Continue,
            };
        }

        match key.code {
            KeyCode::Char('v' | 'V') if !self.receipt_details_open => {
                self.vkm_editor = Some(VkmEditor::start(state));
                self.receipt_details_open = false;
                self.recovery_review_open = false;
                CompanyWorkspaceAction::ClearNotice
            }
            KeyCode::Char('r' | 'R') if !self.receipt_details_open => {
                if self
                    .recovery_selection
                    .selected_destination(state)
                    .is_some()
                {
                    self.recovery_review_open = true;
                    self.receipt_details_open = false;
                    CompanyWorkspaceAction::ClearNotice
                } else {
                    CompanyWorkspaceAction::Continue
                }
            }
            KeyCode::Enter => {
                if self.receipt_selection.has_selection(state) {
                    self.receipt_details_open = true;
                    CompanyWorkspaceAction::ClearNotice
                } else {
                    CompanyWorkspaceAction::Continue
                }
            }
            KeyCode::Esc if self.receipt_details_open => {
                self.receipt_details_open = false;
                CompanyWorkspaceAction::Continue
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if !self.receipt_details_open =>
            {
                self.receipt_selection.handle_key(key.code, state);
                CompanyWorkspaceAction::Continue
            }
            _ => CompanyWorkspaceAction::Continue,
        }
    }

    /// Renders the Company operational dashboard with workspace-owned receipt selection.
    pub fn render_dashboard(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_dashboard(
            frame,
            area,
            state,
            &mut self.receipt_selection,
            self.receipt_details_open,
        );
    }

    /// Renders whichever Company-owned focused workflow currently has input.
    pub fn render_modal(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(editor) = &self.vkm_editor {
            render_vkm_editor(frame, area, editor, state);
        } else if self.recovery_review_open {
            render_recovery_review(frame, area, state, &mut self.recovery_selection);
        } else if self.receipt_details_open {
            render_receipt_modal(frame, area, state, &mut self.receipt_selection);
        }
    }

    /// Closes the editor only after the application boundary persisted the new VKM.
    pub fn confirm_vkm_saved(&mut self) {
        self.vkm_editor = None;
    }

    /// Text fallback used by very small terminals.
    pub fn render_text(&self, state: &GameState) -> String {
        render(state)
    }
}

/// Renders the Company workspace as one operational dashboard. Wide layouts
/// group status, Fleet, operations, identity, financial performance, and Journey
/// history inside one focused shell; compact layouts preserve the same hierarchy.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    _receipt_details_open: bool,
) {
    selection.synchronize(state);
    if area.width >= 100 && area.height >= 20 {
        render_wide_dashboard(frame, area, state, selection);
    } else if area.width >= 76 && area.height >= 12 {
        render_compact_dashboard(frame, area, state, selection);
    } else {
        render_tiny_dashboard(frame, area, state);
    }
}

/// Renders the calculated recovery routes as a focused review modal.
/// The modal never performs a sale, purchase, or dispatch itself; Enter only
/// moves the Player to the workspace where that action can be reviewed.
pub fn render_recovery_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut RecoverySelection,
) {
    let card = modal::workflow_rect(area);
    let compact = card.width < 76;
    let footer = if compact {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Route),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Route),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Page),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ])
    };
    let modal_areas = modal::render_shell(frame, card, "Financial Recovery", footer);

    let Some(evaluation) = recovery_evaluation(state) else {
        frame.render_widget(
            Paragraph::new(vec![
                section_heading("RECOVERY UNAVAILABLE"),
                Line::from(""),
                Line::styled(
                    "Finite recovery routes are available only while the Player Company is Insolvent.",
                    theme::secondary(),
                ),
            ])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            modal_areas.body,
        );
        return;
    };

    selection.synchronize(evaluation.recovery_options.len());
    if compact || modal_areas.body.height < 12 {
        let [routes_area, separator_area, detail_area] = Layout::vertical([
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(modal_areas.body);
        render_recovery_routes(frame, routes_area, state, &evaluation, selection);
        modal::render_horizontal_separator(frame, separator_area);
        render_recovery_instructions(frame, detail_area, state, &evaluation, selection);
    } else {
        let [routes_area, separator_area, detail_area] = Layout::horizontal([
            Constraint::Length(34),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(modal_areas.body);
        render_recovery_routes(frame, routes_area, state, &evaluation, selection);
        modal::render_vertical_separator(frame, separator_area);
        render_recovery_instructions(frame, detail_area, state, &evaluation, selection);
    }
}

/// Renders the selected retained Journey receipt as a focused modal over the
/// Company dashboard. The receipt selection remains owned by the history list.
pub fn render_receipt_modal(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let receipt = selection.selected_receipt(state);
    let title = receipt.map_or_else(
        || "Journey Receipt".to_owned(),
        |receipt| format!("Journey Receipt · J{:02}", receipt.journey_id.get()),
    );
    let card = modal::centered_rect(area, 76, 18);
    let modal_areas = modal::render_shell(
        frame,
        card,
        &title,
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )]),
    );
    let lines = receipt_detail_lines(state, receipt);
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

fn recovery_evaluation(state: &GameState) -> Option<FinancialEvaluation> {
    evaluate_financial_recovery(state)
        .ok()
        .filter(|evaluation| {
            evaluation.status == FinancialStatus::Insolvent
                && !evaluation.recovery_options.is_empty()
        })
}

fn render_recovery_routes(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &FinancialEvaluation,
    selection: &mut RecoverySelection,
) {
    let [heading_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading("FINITE RECOVERY ROUTES")).style(theme::panel()),
        heading_area,
    );
    selection.set_page_size(usize::from(list_area.height).max(1));
    let routes = evaluation
        .recovery_options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            ListItem::new(Line::styled(
                format!("{:>2}. {}", index + 1, recovery_route_label(state, option)),
                theme::primary_value(),
            ))
        })
        .collect::<Vec<_>>();
    let list = List::new(routes)
        .highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, list_area, &mut selection.list_state);
}

fn render_recovery_instructions(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &FinancialEvaluation,
    selection: &mut RecoverySelection,
) {
    let Some(option) = selection
        .list_state
        .selected()
        .and_then(|index| evaluation.recovery_options.get(index))
    else {
        return;
    };
    let destination = selection
        .selected_destination(state)
        .expect("a selected recovery route has a destination");
    let mut lines = vec![
        section_heading("ROUTE DETAILS"),
        Line::styled(
            format!(
                "Route {} · {}",
                selection.list_state.selected().unwrap_or(0) + 1,
                recovery_route_label(state, option)
            ),
            theme::title(),
        ),
        Line::from(""),
    ];
    lines.extend(recovery_steps(state, option));
    lines.push(Line::from(""));
    lines.push(Line::styled(
        format!(
            "Next review: {}. Nothing is sold, bought, or dispatched from this modal.",
            recovery_destination_label(destination)
        ),
        theme::secondary(),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn recovery_route_label(state: &GameState, option: &RecoveryOption) -> String {
    match option {
        RecoveryOption::CashOnly { journey } => format!(
            "Dispatch Train {} to {}",
            journey.train_id.get(),
            station_label(state, journey.destination_station_id),
        ),
        RecoveryOption::SellOthersAndRetain {
            retained_train_id, ..
        } => format!("Retain Train {}; sell others", retained_train_id.get()),
        RecoveryOption::SellAllAndRebuy {
            catalogue_index, ..
        } => {
            let catalogue_name = train_catalogue()
                .models()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            format!("Sell Fleet; rebuy {catalogue_name}")
        }
    }
}

fn recovery_steps(state: &GameState, option: &RecoveryOption) -> Vec<Line<'static>> {
    match option {
        RecoveryOption::CashOnly { journey } => vec![
            recovery_step(
                1,
                format!(
                    "Review Manual Dispatch of Train {} from {} to {}.",
                    journey.train_id.get(),
                    station_label(state, journey.origin_station_id),
                    station_label(state, journey.destination_station_id),
                ),
            ),
            recovery_step(
                2,
                format!(
                    "Confirm only if the displayed operating cost of {} is still affordable.",
                    format_money(journey.operating_cost),
                ),
            ),
        ],
        RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            journey,
        } => vec![
            recovery_step(1, format!("Retain Train {}.", retained_train_id.get())),
            recovery_step(
                2,
                format!(
                    "Review resale of {} for {} in Fleet.",
                    train_ids_label(sold_train_ids),
                    format_money(*resale_proceeds),
                ),
            ),
            recovery_step(
                3,
                format!(
                    "Review Manual Dispatch of retained Train {} from {} to {} for {}.",
                    journey.train_id.get(),
                    station_label(state, journey.origin_station_id),
                    station_label(state, journey.destination_station_id),
                    format_money(journey.operating_cost),
                ),
            ),
        ],
        RecoveryOption::SellAllAndRebuy {
            sold_train_ids,
            resale_proceeds,
            catalogue_index,
            delivery_station_id,
            journey,
        } => {
            let catalogue_name = train_catalogue()
                .models()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            vec![
                recovery_step(
                    1,
                    format!(
                        "Review resale of {} for {} in Fleet.",
                        train_ids_label(sold_train_ids),
                        format_money(*resale_proceeds),
                    ),
                ),
                recovery_step(
                    2,
                    format!(
                        "Review purchase of {catalogue_name} delivered to {} in Market.",
                        station_label(state, *delivery_station_id),
                    ),
                ),
                recovery_step(
                    3,
                    format!(
                        "Review Manual Dispatch from {} to {} for {} after the replacement is READY.",
                        station_label(state, journey.origin_station_id),
                        station_label(state, journey.destination_station_id),
                        format_money(journey.operating_cost),
                    ),
                ),
            ]
        }
    }
}

fn recovery_step(number: usize, text: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{number}. "), theme::warning().bold()),
        Span::styled(text, theme::primary_value()),
    ])
}

fn recovery_destination_label(destination: RecoveryDestination) -> &'static str {
    match destination {
        RecoveryDestination::Fleet => "Fleet",
        RecoveryDestination::BuyTrains => "Market",
        RecoveryDestination::Map => "Map",
    }
}

fn render_wide_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let evaluation = evaluate_financial_recovery(state);

    // Company is an executive dashboard first and a ledger second. Keep the
    // most important financial signals above the operating footprint so a
    // player can understand the Company's state before scanning Journey detail.
    let shell = components::panel_block("Company", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let [overview_area, metrics_area, operations_area, history_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(shell_inner);

    render_company_overview(frame, overview_area, state, &evaluation);
    render_key_metrics(frame, metrics_area, state);
    render_operating_summary(frame, operations_area, state);

    let recovery_relevant = evaluation.as_ref().map_or(true, |evaluation| {
        evaluation.status != FinancialStatus::Operating
    });
    if recovery_relevant {
        let [receipts_area, recovery_area] =
            Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)])
                .spacing(2)
                .areas(history_area);
        render_receipts_table(frame, receipts_area, state, selection, true);
        render_recovery_panel(frame, recovery_area, state, &evaluation);
    } else {
        render_receipts_table(frame, history_area, state, selection, true);
    }
}

fn render_company_overview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let [status_area, identity_area] =
        Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)])
            .spacing(2)
            .areas(area);

    let status_lines = match evaluation {
        Ok(evaluation) => vec![
            Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)).bold(),
            ),
            Line::styled(status_explanation(evaluation.status), theme::secondary()),
        ],
        Err(error) => vec![
            Line::styled("[?] STATUS UNAVAILABLE", theme::error().bold()),
            Line::styled(
                format!("Financial evaluation unavailable: {error}"),
                theme::error(),
            ),
        ],
    };
    render_dashboard_section(frame, status_area, status_lines);

    let registration = &state.region.railway_registration;
    render_dashboard_section(
        frame,
        identity_area,
        vec![
            Line::from(vec![
                Span::styled(
                    state.player_company.vehicle_keeper_mark.as_str().to_owned(),
                    theme::primary_value().bold(),
                ),
                Span::styled("  ·  Rail ", theme::secondary()),
                Span::styled(
                    format!("{} · {}", registration.display_code(), registration.mark),
                    theme::primary_value(),
                ),
            ]),
            Line::styled(state.region.name.clone(), theme::secondary()),
        ],
    );
}

fn render_key_metrics(frame: &mut Frame, area: Rect, state: &GameState) {
    let result = operating_result_cents(state);
    let operating_costs = operating_costs_cents(state);
    let [result_area, revenue_area, costs_area, margin_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    render_key_metric_card(
        frame,
        result_area,
        "OPERATING RESULT",
        format_signed_cents(result),
        result_style(result),
    );
    render_key_metric_card(
        frame,
        revenue_area,
        "REVENUE",
        format_money(state.financials.operating_revenue),
        theme::primary_value(),
    );
    render_key_metric_card(
        frame,
        costs_area,
        "OPERATING COSTS",
        format_cents(operating_costs),
        theme::primary_value(),
    );
    render_key_metric_card(
        frame,
        margin_area,
        "MARGIN",
        operating_margin_label(state),
        result_style(result),
    );
}

fn render_key_metric_card(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: String,
    value_style: Style,
) {
    let block = components::panel_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(value, value_style.bold()),
            Line::styled("lifetime", theme::secondary()),
        ])
        .alignment(Alignment::Center)
        .style(theme::panel()),
        inner,
    );
}

fn render_operating_summary(frame: &mut Frame, area: Rect, state: &GameState) {
    let [fleet_area, services_area, network_area, costs_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(2)
    .areas(area);

    let trains = &state.player_company.fleet.trains;
    render_dashboard_section(
        frame,
        fleet_area,
        vec![
            section_heading("FLEET"),
            Line::styled(format!("{} trains", trains.len()), theme::primary_value()),
            Line::from(vec![
                Span::styled("Fleet value  ", theme::secondary()),
                Span::styled(
                    format_cents(fleet_value_cents(state)),
                    theme::primary_value(),
                ),
            ]),
        ],
    );

    let defined_services = state.player_company.passenger_services.len();
    let active_services = active_service_count(state);
    render_dashboard_section(
        frame,
        services_area,
        vec![
            section_heading("SERVICES"),
            Line::styled(
                format!("{active_services} / {defined_services} active"),
                if active_services > 0 {
                    theme::success()
                } else {
                    theme::primary_value()
                },
            ),
            Line::from(vec![
                Span::styled("Idle  ", theme::secondary()),
                Span::styled(
                    defined_services.saturating_sub(active_services).to_string(),
                    theme::primary_value(),
                ),
            ]),
        ],
    );

    let served_settlements = served_settlement_count(state);
    let connected_settlements = connected_settlement_count(state);
    render_dashboard_section(
        frame,
        network_area,
        vec![
            section_heading("NETWORK"),
            Line::styled(
                format!("{served_settlements} / {connected_settlements} served"),
                theme::primary_value(),
            ),
            Line::styled(
                settlement_coverage_label(served_settlements, connected_settlements),
                theme::secondary(),
            ),
        ],
    );

    render_dashboard_section(
        frame,
        costs_area,
        vec![
            section_heading("COST MIX"),
            dashboard_line(
                "Access",
                format_money(state.financials.infrastructure_access_fees),
                theme::primary_value(),
            ),
            dashboard_line(
                "Fuel",
                format_money(state.financials.fuel_costs),
                theme::primary_value(),
            ),
        ],
    );
}

fn active_service_count(state: &GameState) -> usize {
    state
        .player_company
        .passenger_services
        .iter()
        .filter(|service| {
            state
                .active_journeys
                .iter()
                .any(|journey| journey.service_id == service.id)
        })
        .count()
}

fn connected_settlement_count(state: &GameState) -> usize {
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.settlement_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn served_settlement_count(state: &GameState) -> usize {
    let network = &state.region.rail_authority.rail_network;
    state
        .player_company
        .passenger_services
        .iter()
        .flat_map(|service| service.stop_station_ids.iter().copied())
        .filter_map(|station_id| {
            network
                .rail_stations
                .iter()
                .find(|station| station.id == station_id)
                .map(|station| station.settlement_id)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn settlement_coverage_label(served: usize, connected: usize) -> String {
    if connected == 0 {
        return "—".into();
    }
    let percent = served.saturating_mul(100) / connected;
    format!("{served} / {connected} · {percent}%")
}

fn render_dashboard_section(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_compact_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let evaluation = evaluate_financial_recovery(state);
    let shell = components::panel_block("Company", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let [summary_area, history_area] =
        Layout::horizontal([Constraint::Percentage(48), Constraint::Fill(1)])
            .spacing(2)
            .areas(shell_inner);
    render_compact_summary(frame, summary_area, state, &evaluation);
    render_receipts_table(frame, history_area, state, selection, false);
}

fn render_tiny_dashboard(frame: &mut Frame, area: Rect, state: &GameState) {
    let evaluation = evaluate_financial_recovery(state);
    let shell = components::panel_block("Company", true);
    let inner = shell.inner(area);
    frame.render_widget(shell, area);

    let status = evaluation
        .as_ref()
        .map_or("[?] STATUS UNAVAILABLE".to_owned(), |evaluation| {
            status_label(evaluation.status).to_owned()
        });
    let result = operating_result_cents(state);
    let lines = vec![
        section_heading("STATUS"),
        Line::styled(
            status,
            status_style(evaluation.as_ref().ok().map(|evaluation| evaluation.status)),
        ),
        Line::from(""),
        section_heading("COMPANY"),
        dashboard_line(
            "VKM",
            state.player_company.vehicle_keeper_mark.as_str().to_owned(),
            theme::primary_value(),
        ),
        dashboard_line(
            "Fleet value",
            format_cents(fleet_value_cents(state)),
            theme::primary_value(),
        ),
        dashboard_line("Result", format_signed_cents(result), result_style(result)),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_receipts_table(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    wide: bool,
) {
    let receipts = &state.financials.recent_journey_receipts;
    let [heading_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading(&format!(
            "JOURNEY HISTORY · {} RECEIPTS",
            receipts.len()
        )))
        .style(theme::panel()),
        heading_area,
    );

    if receipts.is_empty() {
        frame.render_widget(
            Paragraph::new("No retained Journey receipts yet. Operating Revenue is credited as passengers reach their stops; a Journey receipt is retained at the Service terminus.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            content_area,
        );
        return;
    }

    let visible_items = usize::from(content_area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);
    let detailed = wide && content_area.width >= 86;
    let rows = receipts.iter().rev().map(|receipt| {
        let result = receipt_result_cents(
            receipt.revenue,
            receipt.infrastructure_access_fee,
            receipt.fuel_cost,
        );
        if detailed {
            Row::new([
                Cell::from(format!("J{:02}", receipt.journey_id.get())),
                Cell::from(receipt_route_label(state, receipt)),
                Cell::from(receipt_train_label(receipt)),
                Cell::from(receipt_passenger_label(receipt)),
                Cell::from(receipt_age_label(state, receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        } else {
            Row::new([
                Cell::from(receipt_route_or_id_label(state, receipt)),
                Cell::from(receipt_passenger_label(receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        }
    });
    let (header, widths) = if detailed {
        (
            Row::new(["ID", "Route", "Train", "Pax", "Completed", "Result"]),
            vec![
                Constraint::Length(6),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(9),
                Constraint::Length(11),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(["Journey", "Pax", "Result"]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(9),
                Constraint::Length(12),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(header.style(theme::table_header()).bottom_margin(1))
        .style(theme::panel())
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, content_area, &mut selection.table_state);
}

fn render_recovery_panel(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let [heading_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading("FINANCIAL WARNING")).style(theme::panel()),
        heading_area,
    );
    let lines = match evaluation {
        Ok(evaluation) => {
            let mut lines = vec![Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            )];
            match evaluation.status {
                FinancialStatus::Operating => lines.push(Line::styled(
                    "No recovery action is required.",
                    theme::secondary(),
                )),
                FinancialStatus::BankruptcyDeferred => lines.push(Line::styled(
                    "An active Journey may still settle revenue.",
                    theme::secondary(),
                )),
                FinancialStatus::Insolvent => {
                    lines.push(Line::styled(
                        "Concrete recovery options:",
                        theme::secondary(),
                    ));
                    lines.push(Line::styled(
                        "Only routes affordable under the current rules are listed; review before acting.",
                        theme::hint(),
                    ));
                    lines.extend(
                        evaluation
                            .recovery_options
                            .iter()
                            .take(MAXIMUM_RECOVERY_OPTIONS_SHOWN)
                            .map(|option| {
                                Line::styled(
                                    format!("· {}", recovery_option_description(state, option)),
                                    theme::primary_value(),
                                )
                            }),
                    );
                }
                FinancialStatus::Bankruptcy => lines.push(Line::styled(
                    "No finite recovery option remains.",
                    theme::secondary(),
                )),
            }
            lines
        }
        Err(error) => vec![Line::styled(
            format!("Recovery unavailable: {error}"),
            theme::error(),
        )],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        content_area,
    );
}

fn render_compact_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let result = operating_result_cents(state);
    let mut lines = vec![
        section_heading("STATUS"),
        match evaluation {
            Ok(evaluation) => Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            ),
            Err(error) => Line::styled(format!("[?] STATUS UNAVAILABLE: {error}"), theme::error()),
        },
        Line::from(""),
        section_heading("COMPANY"),
        dashboard_line(
            "VKM",
            state.player_company.vehicle_keeper_mark.as_str().to_owned(),
            theme::primary_value(),
        ),
        dashboard_line(
            "Fleet value",
            format_cents(fleet_value_cents(state)),
            theme::primary_value(),
        ),
        dashboard_line(
            "Services",
            format!(
                "{} · {} active",
                state.player_company.passenger_services.len(),
                active_service_count(state),
            ),
            theme::primary_value(),
        ),
        dashboard_line(
            "Coverage",
            format!(
                "{} places",
                settlement_coverage_label(
                    served_settlement_count(state),
                    connected_settlement_count(state),
                )
            ),
            theme::primary_value(),
        ),
        Line::from(""),
        section_heading("FINANCIAL · LIFETIME"),
        dashboard_line(
            "Revenue",
            format_money(state.financials.operating_revenue),
            theme::primary_value(),
        ),
        dashboard_line(
            "Operating costs",
            format_cents(operating_costs_cents(state)),
            theme::primary_value(),
        ),
        dashboard_line(
            "Operating result",
            format_signed_cents(result),
            result_style(result),
        ),
        dashboard_line(
            "Margin",
            operating_margin_label(state),
            result_style(result),
        ),
    ];
    if let Ok(evaluation) = evaluation
        && let Some(option) = evaluation.recovery_options.first()
    {
        lines.push(Line::from(""));
        lines.push(section_heading("RECOVERY"));
        lines.push(Line::styled(
            compact_recovery_description(state, option),
            theme::primary_value(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn receipt_detail_lines(state: &GameState, receipt: Option<&JourneyReceipt>) -> Vec<Line<'static>> {
    match receipt {
        Some(receipt) => {
            let result = receipt_result_cents(
                receipt.revenue,
                receipt.infrastructure_access_fee,
                receipt.fuel_cost,
            );
            let mut lines = vec![
                Line::styled(
                    format!(
                        "Journey {} · {}",
                        receipt.journey_id.get(),
                        receipt_age_label(state, receipt)
                    ),
                    theme::title(),
                ),
                Line::from(""),
            ];
            if receipt_has_operating_context(receipt) {
                lines.push(section_heading("JOURNEY"));
                lines.extend([
                    financial_line(
                        "Route",
                        receipt_route_label(state, receipt),
                        theme::primary_value(),
                    ),
                    financial_line(
                        "Train",
                        receipt_train_label(receipt),
                        theme::primary_value(),
                    ),
                    financial_line(
                        "Passengers",
                        receipt_passenger_detail(receipt),
                        theme::primary_value(),
                    ),
                    Line::from(""),
                ]);
            } else {
                lines.push(Line::styled(
                    "Operating context is unavailable for this legacy receipt.",
                    theme::secondary(),
                ));
                lines.push(Line::from(""));
            }
            lines.push(section_heading("FINANCIAL"));
            lines.extend([
                financial_line(
                    "Revenue",
                    format_money(receipt.revenue),
                    theme::primary_value(),
                ),
                financial_line(
                    "Access fees",
                    format_money(receipt.infrastructure_access_fee),
                    theme::primary_value(),
                ),
                financial_line(
                    "Fuel",
                    format_money(receipt.fuel_cost),
                    theme::primary_value(),
                ),
                financial_line("Result", format_signed_cents(result), result_style(result)),
            ]);
            lines
        }
        None => vec![Line::styled(
            "The selected receipt is no longer retained.",
            theme::secondary(),
        )],
    }
}

fn section_heading(title: &str) -> Line<'static> {
    Line::styled(title.to_owned(), theme::secondary().bold())
}

fn dashboard_line(label: &str, value: String, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<14}"), theme::secondary()),
        Span::styled(value, value_style),
    ])
}

fn financial_line(label: &str, value: String, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<20}"), theme::secondary()),
        Span::styled(value, value_style),
    ])
}

fn status_label(status: FinancialStatus) -> &'static str {
    match status {
        FinancialStatus::Operating => "[OK] OPERATING",
        FinancialStatus::Insolvent => "[!] INSOLVENT",
        FinancialStatus::BankruptcyDeferred => "[~] SETTLEMENT PENDING",
        FinancialStatus::Bankruptcy => "[X] BANKRUPT",
    }
}

fn status_explanation(status: FinancialStatus) -> &'static str {
    match status {
        FinancialStatus::Operating => {
            "Working capital is sufficient to continue normal operations."
        }
        FinancialStatus::Insolvent => "No Journey can be funded without a recovery action.",
        FinancialStatus::BankruptcyDeferred => {
            "No Journey can be funded now; active Journey revenue is still unsettled."
        }
        FinancialStatus::Bankruptcy => {
            "No finite recovery path can return the Player Company to operation."
        }
    }
}

fn status_style(status: Option<FinancialStatus>) -> Style {
    match status {
        Some(FinancialStatus::Operating) => theme::success().bold(),
        Some(FinancialStatus::Insolvent | FinancialStatus::BankruptcyDeferred) => {
            theme::warning().bold()
        }
        Some(FinancialStatus::Bankruptcy) | None => theme::error().bold(),
    }
}

fn result_style(result: i128) -> Style {
    if result < 0 {
        theme::warning().bold()
    } else {
        theme::success().bold()
    }
}

fn operating_result_cents(state: &GameState) -> i128 {
    receipt_result_cents(
        state.financials.operating_revenue,
        state.financials.infrastructure_access_fees,
        state.financials.fuel_costs,
    )
}

fn operating_costs_cents(state: &GameState) -> i128 {
    i128::from(state.financials.infrastructure_access_fees.cents())
        + i128::from(state.financials.fuel_costs.cents())
}

fn operating_margin_label(state: &GameState) -> String {
    let revenue = i128::from(state.financials.operating_revenue.cents());
    if revenue == 0 {
        return "—".into();
    }

    let tenths = operating_result_cents(state).saturating_mul(1_000) / revenue;
    let absolute = tenths.abs();
    let sign = if tenths > 0 {
        "+"
    } else if tenths < 0 {
        "-"
    } else {
        ""
    };
    format!("{sign}{}.{:01}%", absolute / 10, absolute % 10)
}

fn receipt_result_cents(revenue: Money, access_fees: Money, fuel_costs: Money) -> i128 {
    i128::from(revenue.cents()) - i128::from(access_fees.cents()) - i128::from(fuel_costs.cents())
}

fn compact_recovery_description(_state: &GameState, option: &RecoveryOption) -> String {
    match option {
        RecoveryOption::CashOnly { journey } => format!(
            "Dispatch Train {} · {}",
            journey.train_id.get(),
            format_money(journey.operating_cost)
        ),
        RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            ..
        } => format!(
            "Keep Train {}; sell {} · +{}",
            retained_train_id.get(),
            train_ids_label(sold_train_ids),
            format_money(*resale_proceeds)
        ),
        RecoveryOption::SellAllAndRebuy {
            sold_train_ids,
            resale_proceeds,
            catalogue_index,
            ..
        } => {
            let name = train_catalogue()
                .models()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            format!(
                "Sell {}; buy {name} · +{}",
                train_ids_label(sold_train_ids),
                format_money(*resale_proceeds)
            )
        }
    }
}

/// Renders the Player Company's funds, operating totals, receipts, and
/// financial recovery status.
pub fn render(state: &GameState) -> String {
    let mut output = String::new();
    let financials = &state.financials;

    writeln!(output, "{}", state.player_company.name).expect("writing to a String cannot fail");
    writeln!(
        output,
        "Vehicle Keeper Mark (VKM): {}",
        state.player_company.vehicle_keeper_mark
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
        "Fleet value: {} (original purchase prices)",
        format_cents(fleet_value_cents(state)),
    )
    .expect("writing to a String cannot fail");

    writeln!(output, "\nOperating totals:").expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Operating Revenue: {}",
        format_money(financials.operating_revenue),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Infrastructure Access Fee: {}",
        format_money(financials.infrastructure_access_fees),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Fuel Cost: {}",
        format_money(financials.fuel_costs)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Journey Profitability total: {}",
        format_cents(
            i128::from(financials.operating_revenue.cents())
                - i128::from(financials.infrastructure_access_fees.cents())
                - i128::from(financials.fuel_costs.cents()),
        ),
    )
    .expect("writing to a String cannot fail");

    render_receipts(&mut output, state);
    render_financial_status(&mut output, state);
    output
}

fn render_receipts(output: &mut String, state: &GameState) {
    let receipts = &state.financials.recent_journey_receipts;
    writeln!(output, "\nLatest receipts:").expect("writing to a String cannot fail");
    if receipts.is_empty() {
        writeln!(output, "  No Journey receipts have settled yet.")
            .expect("writing to a String cannot fail");
        return;
    }

    for receipt in receipts.iter().rev() {
        let profitability = i128::from(receipt.revenue.cents())
            - i128::from(receipt.infrastructure_access_fee.cents())
            - i128::from(receipt.fuel_cost.cents());
        writeln!(
            output,
            "  Journey {} — {}; {}; Passengers: {}; Revenue: {}; Access: {}; Fuel: {}; Result: {}",
            receipt.journey_id.get(),
            receipt_route_label(state, receipt),
            receipt_train_label(receipt),
            receipt_passenger_label(receipt),
            format_money(receipt.revenue),
            format_money(receipt.infrastructure_access_fee),
            format_money(receipt.fuel_cost),
            format_cents(profitability),
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_financial_status(output: &mut String, state: &GameState) {
    writeln!(output, "\nFinancial status:").expect("writing to a String cannot fail");
    let evaluation = match evaluate_financial_recovery(state) {
        Ok(evaluation) => evaluation,
        Err(error) => {
            writeln!(
                output,
                "  Financial recovery evaluation unavailable: {error}"
            )
            .expect("writing to a String cannot fail");
            return;
        }
    };

    match evaluation.status {
        FinancialStatus::Operating => writeln!(
            output,
            "  Operating: Company Funds can cover at least one available Journey.",
        )
        .expect("writing to a String cannot fail"),
        FinancialStatus::BankruptcyDeferred => writeln!(
            output,
            "  Insolvency assessment deferred: an active Journey may still settle Operating Revenue before Bankruptcy is considered.",
        )
        .expect("writing to a String cannot fail"),
        FinancialStatus::Insolvent => {
            writeln!(
                output,
                "  INSOLVENCY: Company Funds cannot currently cover an available Journey, but a finite recovery option remains.",
            )
            .expect("writing to a String cannot fail");
            render_recovery_options(output, state, &evaluation.recovery_options);
        }
        FinancialStatus::Bankruptcy => writeln!(
            output,
            "  BANKRUPTCY: no finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
        )
        .expect("writing to a String cannot fail"),
    }
}

fn render_recovery_options(output: &mut String, state: &GameState, options: &[RecoveryOption]) {
    writeln!(output, "  Recovery options:").expect("writing to a String cannot fail");
    for option in options.iter().take(MAXIMUM_RECOVERY_OPTIONS_SHOWN) {
        writeln!(
            output,
            "    - {}",
            recovery_option_description(state, option)
        )
        .expect("writing to a String cannot fail");
    }
    if options.len() > MAXIMUM_RECOVERY_OPTIONS_SHOWN {
        writeln!(
            output,
            "    Showing {} of {} finite recovery options.",
            MAXIMUM_RECOVERY_OPTIONS_SHOWN,
            options.len(),
        )
        .expect("writing to a String cannot fail");
    }
}

fn recovery_option_description(state: &GameState, option: &RecoveryOption) -> String {
    match option {
        RecoveryOption::CashOnly { journey } => format!(
            "Dispatch Train {} from {} to {} for {}.",
            journey.train_id.get(),
            station_label(state, journey.origin_station_id),
            station_label(state, journey.destination_station_id),
            format_money(journey.operating_cost),
        ),
        RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            journey,
        } => format!(
            "Keep Train {}; sell {} for {}; then {}",
            retained_train_id.get(),
            train_ids_label(sold_train_ids),
            format_money(*resale_proceeds),
            journey_description(state, journey),
        ),
        RecoveryOption::SellAllAndRebuy {
            sold_train_ids,
            resale_proceeds,
            catalogue_index,
            delivery_station_id,
            journey,
        } => {
            let catalogue_name = train_catalogue()
                .models()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            format!(
                "Sell {} for {}; buy {catalogue_name} at {}; then {}",
                train_ids_label(sold_train_ids),
                format_money(*resale_proceeds),
                station_label(state, *delivery_station_id),
                journey_description(state, journey),
            )
        }
    }
}

fn journey_description(state: &GameState, journey: &RecoveryJourney) -> String {
    format!(
        "dispatch Train {} from {} to {} for {}.",
        journey.train_id.get(),
        station_label(state, journey.origin_station_id),
        station_label(state, journey.destination_station_id),
        format_money(journey.operating_cost),
    )
}

fn train_ids_label(train_ids: &[crate::model::TrainId]) -> String {
    match train_ids {
        [] => "no Trains".into(),
        [train_id] => format!("Train {}", train_id.get()),
        _ => format!(
            "Trains {}",
            train_ids
                .iter()
                .map(|train_id| train_id.get().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn fleet_value_cents(state: &GameState) -> i128 {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| i128::from(train.original_purchase_price.cents()))
        .sum()
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

fn receipt_has_operating_context(receipt: &JourneyReceipt) -> bool {
    receipt.train_id.is_some()
        && receipt.train_model_name.is_some()
        && receipt.origin_station_id.is_some()
        && receipt.destination_station_id.is_some()
        && receipt.passengers_carried.is_some()
        && receipt.passenger_capacity.is_some()
        && receipt.completed_at.is_some()
}

fn receipt_route_label(state: &GameState, receipt: &JourneyReceipt) -> String {
    match (receipt.origin_station_id, receipt.destination_station_id) {
        (Some(origin), Some(destination)) => format!(
            "{} → {}",
            station_label(state, origin),
            station_label(state, destination)
        ),
        _ => "Legacy receipt".into(),
    }
}

fn receipt_route_or_id_label(state: &GameState, receipt: &JourneyReceipt) -> String {
    if receipt.origin_station_id.is_some() && receipt.destination_station_id.is_some() {
        receipt_route_label(state, receipt)
    } else {
        format!("Journey {}", receipt.journey_id.get())
    }
}

fn receipt_train_label(receipt: &JourneyReceipt) -> String {
    match (receipt.train_id, receipt.train_model_name.as_deref()) {
        (Some(train_id), Some(model_name)) => format!("T{} · {model_name}", train_id.get()),
        (Some(train_id), None) => format!("Train {}", train_id.get()),
        (None, Some(model_name)) => model_name.to_owned(),
        (None, None) => "—".into(),
    }
}

fn receipt_passenger_label(receipt: &JourneyReceipt) -> String {
    match (receipt.passengers_carried, receipt.passenger_capacity) {
        (Some(passengers), Some(capacity)) => format!("{passengers} boarded · {capacity} seats"),
        (Some(passengers), None) => format!("{passengers} boarded"),
        _ => "—".into(),
    }
}

fn receipt_passenger_detail(receipt: &JourneyReceipt) -> String {
    receipt_passenger_label(receipt)
}

fn receipt_age_label(state: &GameState, receipt: &JourneyReceipt) -> String {
    let Some(completed_at) = receipt.completed_at else {
        return "legacy".into();
    };
    let elapsed = state
        .last_processed_at
        .unix_seconds()
        .saturating_sub(completed_at.unix_seconds())
        .max(0) as u64;
    match elapsed {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h {}m ago", elapsed / 3_600, (elapsed % 3_600) / 60),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

fn format_money(money: Money) -> String {
    format_cents(i128::from(money.cents()))
}

fn format_signed_cents(cents: i128) -> String {
    crate::ui::format::signed_cents(cents)
}

fn format_cents(cents: i128) -> String {
    crate::ui::format::signed_cents(cents)
        .trim_start_matches('+')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            time::advance_time, world::create_new_game,
        },
    };

    use super::{CompanyWorkspace, render};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);
    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);

    #[test]
    fn company_view_shows_totals_fleet_value_and_latest_receipt() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).unwrap();

        let rendered = render(&state);

        assert!(rendered.contains("Company Funds: $"));
        assert!(rendered.contains("Fleet value: $3,000.00"));
        assert!(rendered.contains("Operating Revenue: $"));
        assert!(rendered.contains("Infrastructure Access Fee: $"));
        assert!(rendered.contains("Fuel Cost: $"));
        assert!(rendered.contains("Journey Profitability total: $"));
        assert!(rendered.contains("Latest receipts:"));
        assert!(rendered.contains("Journey 1 —"));
        assert!(rendered.contains("Passengers:"));
        assert!(rendered.contains("Result:"));
    }

    #[test]
    fn company_view_explains_insolvency_and_lists_recovery_options() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::from_cents(1_000_000);
        purchase_train(&mut state, 0, ORIGIN).unwrap();
        purchase_train(&mut state, 0, ORIGIN).unwrap();
        state.player_company.funds = Money::ZERO;

        let rendered = render(&state);

        assert!(rendered.contains("INSOLVENCY:"));
        assert!(rendered.contains("Recovery options:"));
        assert!(rendered.contains("Keep Train"));
    }

    #[test]
    fn company_view_shows_bankruptcy_when_no_option_exists() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::ZERO;

        let rendered = render(&state);

        assert!(rendered.contains("BANKRUPTCY:"));
        assert!(!rendered.contains("Recovery options:"));
    }

    #[test]
    fn company_workspace_owns_vkm_footer_and_help() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut workspace = CompanyWorkspace::default();

        workspace.handle_key(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            &state,
        );

        let shortcuts = workspace.shortcuts(&state, false, true);
        assert_eq!(shortcuts[0].key, "Enter");
        assert_eq!(shortcuts[0].action, "Save");
        assert!(workspace.help_lines(&state)[0].contains("Company VKM"));
    }
}
