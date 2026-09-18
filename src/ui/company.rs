//! Financial presentation for the Player Company's Company view.
//!
//! This module reads the stored financial totals and the simulation's finite
//! recovery evaluation. It does not mutate the game state or authorise any
//! recovery action.

mod analytics;

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Cell, Chart, Dataset, GraphType, HighlightSpacing, List, ListItem, ListState,
        Paragraph, Row, Table, TableState, Wrap,
    },
};

use analytics::{
    ActiveJourneyExposure, RECENT_JOURNEY_WINDOW, RecentJourneyPerformance,
    ServicePerformance, ServicePerformanceSummary,
};

use crate::{
    catalog::train_catalogue,
    model::{
        GameState, JourneyId, JourneyPurpose, JourneyReceipt, Money, RailStationId,
        ServiceDirectionMode, ServiceId, VehicleKeeperMark,
    },
    sim::finance::{
        FinancialEvaluation, FinancialStatus, RecoveryJourney, RecoveryOption,
        evaluate_financial_recovery,
    },
    ui::{components, modal, theme},
};

const MAXIMUM_RECOVERY_OPTIONS_SHOWN: usize = 3;
const RECENT_ACTIVITY_LIMIT: usize = 5;
const ATTENTION_SERVICE_LIMIT: usize = 2;

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
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Save),
            modal::ModalShortcut::enabled("Backspace", modal::ModalAction::Erase),
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

/// Persistent selection for the recent per-Service performance browser.
///
/// The selected Service is retained by ID so newly settled Journeys can reorder
/// the summary without silently moving the Player to another Service.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServicePerformanceSelection {
    selected_service_id: Option<ServiceId>,
    table_state: TableState,
    page_size: usize,
}

impl ServicePerformanceSelection {
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        let summary = ServicePerformanceSummary::from_state(state);
        self.synchronize_summary(&summary);
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let count = summary.services.len();
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected.saturating_add(1).min(count.saturating_sub(1))
            }
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(count.saturating_sub(1)),
            _ => selected,
        };
        self.select_index(&summary, next);
    }

    fn synchronize(&mut self, state: &GameState) {
        let summary = ServicePerformanceSummary::from_state(state);
        self.synchronize_summary(&summary);
    }

    fn synchronize_summary(&mut self, summary: &ServicePerformanceSummary) {
        let previous_index = self.table_state.selected().unwrap_or(0);
        let selected = self
            .selected_service_id
            .and_then(|service_id| {
                summary
                    .services
                    .iter()
                    .position(|service| service.service_id == service_id)
            })
            .or_else(|| {
                (!summary.services.is_empty())
                    .then_some(previous_index.min(summary.services.len().saturating_sub(1)))
            });
        self.selected_service_id =
            selected.and_then(|index| summary.services.get(index).map(|service| service.service_id));
        if selected.is_none() {
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, summary: &ServicePerformanceSummary, index: usize) {
        let Some(service) = summary.services.get(index) else {
            return;
        };
        self.selected_service_id = Some(service.service_id);
        self.table_state.select(Some(index));
    }

    fn selected_service(&mut self, state: &GameState) -> Option<ServicePerformance> {
        let summary = ServicePerformanceSummary::from_state(state);
        self.synchronize_summary(&summary);
        self.selected_service_id.and_then(|service_id| {
            summary
                .services
                .into_iter()
                .find(|service| service.service_id == service_id)
        })
    }

    fn has_selection(&mut self, state: &GameState) -> bool {
        self.selected_service(state).is_some()
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
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
    receipt_history_open: bool,
    receipt_details_open: bool,
    service_performance_selection: ServicePerformanceSelection,
    service_performance_open: bool,
    service_performance_details_open: bool,
    recovery_selection: RecoverySelection,
    recovery_review_open: bool,
    vkm_editor: Option<VkmEditor>,
}

impl CompanyWorkspace {
    /// Clears transient Company workflows when the primary view is reopened.
    pub fn activate(&mut self) {
        self.receipt_history_open = false;
        self.receipt_details_open = false;
        self.service_performance_open = false;
        self.service_performance_details_open = false;
        self.recovery_review_open = false;
        self.vkm_editor = None;
    }

    /// Clears all transient Company presentation state after a fresh-game restart.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns whether any Company-owned focused workflow is visible.
    pub fn has_modal(&self) -> bool {
        self.vkm_editor.is_some()
            || self.recovery_review_open
            || self.receipt_history_open
            || self.receipt_details_open
            || self.service_performance_open
            || self.service_performance_details_open
    }

    /// Returns whether the VKM editor must receive text input before global shortcuts.
    pub fn has_vkm_editor(&self) -> bool {
        self.vkm_editor.is_some()
    }

    /// Returns whether recovery navigation must be handled before global workspace shortcuts.
    pub fn recovery_review_open(&self) -> bool {
        self.recovery_review_open
    }

    /// Returns whether receipt history/detail owns navigation until it is closed.
    pub fn receipt_browser_open(&self) -> bool {
        self.receipt_history_open || self.receipt_details_open
    }

    /// Returns whether Service performance browsing owns navigation until closed.
    pub fn service_performance_browser_open(&self) -> bool {
        self.service_performance_open || self.service_performance_details_open
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
                CompanyShortcut::enabled("Esc", "Cancel"),
                CompanyShortcut::enabled("Enter", "Save"),
                CompanyShortcut::enabled("Backspace", "Delete"),
            ];
        }

        if self.recovery_review_open {
            let mut items = vec![CompanyShortcut::enabled("Esc", "Back")];
            items.push(CompanyShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Route",
            ));
            if wide {
                items.push(CompanyShortcut::enabled("PgUp/PgDn", "Page"));
            }
            items.push(CompanyShortcut::enabled("Enter", "Review"));
            return items;
        }

        if self.receipt_details_open {
            return vec![CompanyShortcut::enabled("Esc", "Back")];
        }

        if self.receipt_history_open {
            let mut items = vec![CompanyShortcut::enabled("Esc", "Back")];
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
            return items;
        }

        if self.service_performance_details_open {
            return vec![CompanyShortcut::enabled("Esc", "Back")];
        }

        if self.service_performance_open {
            let mut items = vec![CompanyShortcut::enabled("Esc", "Back")];
            if !ServicePerformanceSummary::from_state(state).services.is_empty() {
                items.push(CompanyShortcut::enabled(
                    if compact { "↑↓" } else { "↑↓/JK" },
                    "Service",
                ));
                if wide {
                    items.push(CompanyShortcut::enabled("PgUp/PgDn", "Page"));
                }
                items.push(CompanyShortcut::enabled("Enter", "Inspect"));
            }
            return items;
        }

        let mut items = vec![CompanyShortcut::enabled("H", "History")];
        items.push(CompanyShortcut::enabled("S", "Service performance"));
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
            ];
        }

        if self.receipt_history_open {
            let mut lines = vec![
                "Current · Journey History".into(),
                "Esc Back to Company dashboard".into(),
            ];
            if state.financials.recent_journey_receipts.is_empty() {
                lines.push("No settled Journey receipts yet".into());
            } else {
                lines.extend([
                    "↑↓ / jk Select Journey receipt".into(),
                    "PgUp / PgDn Scroll history".into(),
                    "Enter Inspect receipt".into(),
                ]);
            }
            return lines;
        }

        if self.service_performance_details_open {
            return vec![
                "Current · Service Performance Detail".into(),
                "Esc Back to Service performance".into(),
            ];
        }

        if self.service_performance_open {
            let mut lines = vec![
                "Current · Service Performance".into(),
                "Esc Back to Company dashboard".into(),
            ];
            if ServicePerformanceSummary::from_state(state).services.is_empty() {
                lines.push("No attributable recent Service Journeys yet".into());
            } else {
                lines.extend([
                    "↑↓ / jk Select Service".into(),
                    "PgUp / PgDn Scroll Services".into(),
                    "Enter Inspect Service".into(),
                ]);
            }
            return lines;
        }

        let mut lines = vec![
            "Current · Company".into(),
            "h Open Journey history".into(),
            "s Open Service performance".into(),
            "v Edit Company VKM".into(),
        ];
        if state.financials.recent_journey_receipts.is_empty() {
            lines.extend([
                "No settled Journey receipts yet".into(),
                "1 Return to Map to operate your railway".into(),
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

        if self.service_performance_details_open {
            return match key.code {
                KeyCode::Esc => {
                    self.service_performance_details_open = false;
                    CompanyWorkspaceAction::Continue
                }
                _ => CompanyWorkspaceAction::Continue,
            };
        }

        if self.service_performance_open {
            return match key.code {
                KeyCode::Esc => {
                    self.service_performance_open = false;
                    CompanyWorkspaceAction::Continue
                }
                KeyCode::Enter => {
                    if self.service_performance_selection.has_selection(state) {
                        self.service_performance_details_open = true;
                        CompanyWorkspaceAction::ClearNotice
                    } else {
                        CompanyWorkspaceAction::Continue
                    }
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                    self.service_performance_selection.handle_key(key.code, state);
                    CompanyWorkspaceAction::Continue
                }
                _ => CompanyWorkspaceAction::Continue,
            };
        }

        if self.receipt_details_open {
            return match key.code {
                KeyCode::Esc => {
                    self.receipt_details_open = false;
                    CompanyWorkspaceAction::Continue
                }
                _ => CompanyWorkspaceAction::Continue,
            };
        }

        if self.receipt_history_open {
            return match key.code {
                KeyCode::Esc => {
                    self.receipt_history_open = false;
                    CompanyWorkspaceAction::Continue
                }
                KeyCode::Enter => {
                    if self.receipt_selection.has_selection(state) {
                        self.receipt_details_open = true;
                        CompanyWorkspaceAction::ClearNotice
                    } else {
                        CompanyWorkspaceAction::Continue
                    }
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                    self.receipt_selection.handle_key(key.code, state);
                    CompanyWorkspaceAction::Continue
                }
                _ => CompanyWorkspaceAction::Continue,
            };
        }

        match key.code {
            KeyCode::Char('h' | 'H') => {
                self.receipt_history_open = true;
                self.receipt_details_open = false;
                self.service_performance_open = false;
                self.service_performance_details_open = false;
                self.recovery_review_open = false;
                CompanyWorkspaceAction::ClearNotice
            }
            KeyCode::Char('s' | 'S') => {
                self.service_performance_open = true;
                self.service_performance_details_open = false;
                self.receipt_history_open = false;
                self.receipt_details_open = false;
                self.recovery_review_open = false;
                self.service_performance_selection.synchronize(state);
                CompanyWorkspaceAction::ClearNotice
            }
            KeyCode::Char('v' | 'V') => {
                self.vkm_editor = Some(VkmEditor::start(state));
                self.receipt_history_open = false;
                self.receipt_details_open = false;
                self.service_performance_open = false;
                self.service_performance_details_open = false;
                self.recovery_review_open = false;
                CompanyWorkspaceAction::ClearNotice
            }
            KeyCode::Char('r' | 'R') => {
                if self
                    .recovery_selection
                    .selected_destination(state)
                    .is_some()
                {
                    self.recovery_review_open = true;
                    self.receipt_history_open = false;
                    self.receipt_details_open = false;
                    self.service_performance_open = false;
                    self.service_performance_details_open = false;
                    CompanyWorkspaceAction::ClearNotice
                } else {
                    CompanyWorkspaceAction::Continue
                }
            }
            _ => CompanyWorkspaceAction::Continue,
        }
    }

    /// Renders the Company operational dashboard with workspace-owned receipt selection.
    pub fn render_dashboard(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        render_dashboard(frame, area, state);
    }

    /// Renders whichever Company-owned focused workflow currently has input.
    pub fn render_modal(&mut self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(editor) = &self.vkm_editor {
            render_vkm_editor(frame, area, editor, state);
        } else if self.recovery_review_open {
            render_recovery_review(frame, area, state, &mut self.recovery_selection);
        } else if self.service_performance_details_open {
            render_service_performance_detail(
                frame,
                area,
                state,
                &mut self.service_performance_selection,
            );
        } else if self.service_performance_open {
            render_service_performance_browser(
                frame,
                area,
                state,
                &mut self.service_performance_selection,
            );
        } else if self.receipt_details_open {
            render_receipt_modal(frame, area, state, &mut self.receipt_selection);
        } else if self.receipt_history_open {
            render_receipt_history(frame, area, state, &mut self.receipt_selection);
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
/// group status, Fleet, operations, identity, financial performance, and recent
/// activity inside one focused shell; full Journey history is a separate browser.
pub fn render_dashboard(frame: &mut Frame, area: Rect, state: &GameState) {
    if area.width >= 100 && area.height >= 20 {
        render_wide_dashboard(frame, area, state);
    } else if area.width >= 76 && area.height >= 12 {
        render_compact_dashboard(frame, area, state);
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
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Route),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
            modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Route),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Page),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Review),
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
        |receipt| format!("Journey Receipt · {}", journey_reference_label(receipt.journey_id)),
    );
    let card = modal::centered_rect(area, 84, 24);
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

/// Renders the complete retained receipt ledger as a focused browser. The
/// Company dashboard itself only shows the latest activity rows.
pub fn render_receipt_history(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let card = modal::centered_rect(area, 118, 26);
    let compact = card.width < 88;
    let footer = if state.financials.recent_journey_receipts.is_empty() {
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )])
    } else if compact {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Close),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Inspect),
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Scroll),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Close),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Inspect),
            modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Page),
        ])
    };
    let title = format!(
        "Journey History · {} receipts",
        state.financials.recent_journey_receipts.len()
    );
    let modal_areas = modal::render_shell(frame, card, &title, footer);
    selection.synchronize(state);
    render_receipt_history_table(frame, modal_areas.body, state, selection, !compact);
}

/// Renders the recent per-Service summary as a focused, selectable browser.
pub fn render_service_performance_browser(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ServicePerformanceSelection,
) {
    let summary = ServicePerformanceSummary::from_state(state);
    let card = modal::centered_rect(area, 118, 26);
    let compact = card.width < 96;
    let footer = if summary.services.is_empty() {
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )])
    } else if compact {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Close),
            modal::ModalShortcut::enabled("↑↓", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Inspect),
        ])
    } else {
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Close),
            modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Scroll),
            modal::ModalShortcut::enabled("PgUp/PgDn", modal::ModalAction::Page),
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Inspect),
        ])
    };
    let recent_journeys = summary.attributed_journeys + summary.unattributed_journeys;
    let title = if recent_journeys == 0 {
        "Service Performance".to_owned()
    } else {
        format!("Service Performance · recent {recent_journeys} Journeys")
    };
    let modal_areas = modal::render_shell(frame, card, &title, footer);
    selection.synchronize_summary(&summary);

    if summary.services.is_empty() {
        let message = if summary.unattributed_journeys > 0 {
            "Recent receipts predate Service telemetry. New completed Journeys will populate Service performance."
        } else {
            "No completed Service activity yet. Revenue runs and positioning moves will appear here after they finish."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(theme::secondary())
                .wrap(Wrap { trim: true }),
            modal_areas.body,
        );
        return;
    }

    let note_height = if summary.unattributed_journeys > 0 { 1 } else { 0 };
    let [table_area, note_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(note_height),
    ])
    .areas(modal_areas.body);
    let visible_rows = usize::from(table_area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_rows);

    let rows = summary.services.iter().map(|service| {
        let has_recent_activity = service.journey_count() > 0;
        if compact {
            Row::new([
                Cell::from(service_performance_label(
                    state,
                    service.service_id,
                    &service.service_code,
                )),
                Cell::from(service_operating_status_label(state, service.service_id))
                    .style(service_operating_status_style(state, service.service_id)),
                if has_recent_activity {
                    Cell::from(format_signed_cents(service.result_cents))
                        .style(result_style(service.result_cents))
                } else {
                    Cell::from("—").style(theme::secondary())
                },
            ])
        } else {
            Row::new([
                Cell::from(service_performance_label(
                    state,
                    service.service_id,
                    &service.service_code,
                )),
                Cell::from(service_operating_status_label(state, service.service_id))
                    .style(service_operating_status_style(state, service.service_id)),
                Cell::from(service.revenue_journeys.to_string()),
                Cell::from(service.positioning_journeys.to_string()),
                Cell::from(if service.revenue_journeys == 0 {
                    "—".into()
                } else {
                    service
                        .passengers_carried
                        .map_or_else(|| "—".into(), |passengers| passengers.to_string())
                }),
                Cell::from(if has_recent_activity {
                    format_cents(service.revenue_cents)
                } else {
                    "—".into()
                }),
                Cell::from(if has_recent_activity {
                    format_cents(service.operating_costs_cents)
                } else {
                    "—".into()
                }),
                if has_recent_activity {
                    Cell::from(format_signed_cents(service.result_cents))
                        .style(result_style(service.result_cents))
                } else {
                    Cell::from("—").style(theme::secondary())
                },
            ])
        }
    });
    let (header, widths) = if compact {
        (
            Row::new(["Service", "Status", "Result"]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(11),
                Constraint::Length(13),
            ],
        )
    } else {
        (
            Row::new([
                "Service",
                "Status",
                "Runs",
                "Positioning",
                "Boardings",
                "Revenue",
                "Costs",
                "Result",
            ]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(11),
                Constraint::Length(10),
                Constraint::Length(13),
                Constraint::Length(13),
                Constraint::Length(13),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(header.style(theme::table_header()).bottom_margin(1))
        .style(theme::panel())
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut selection.table_state);

    if summary.unattributed_journeys > 0 {
        frame.render_widget(
            Paragraph::new(format!(
                "{} recent {} excluded because Service telemetry is unavailable.",
                summary.unattributed_journeys,
                if summary.unattributed_journeys == 1 {
                    "receipt"
                } else {
                    "receipts"
                }
            ))
            .style(theme::hint()),
            note_area,
        );
    }
}

/// Renders one Service's recent operating and financial performance.
pub fn render_service_performance_detail(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ServicePerformanceSelection,
) {
    let service = selection.selected_service(state);
    let title = service.as_ref().map_or_else(
        || "Service Performance".to_owned(),
        |service| format!("Service Performance · {}", service.service_code),
    );
    let card = modal::centered_rect(area, 88, 25);
    let modal_areas = modal::render_shell(
        frame,
        card,
        &title,
        modal::shortcut_line(&[modal::ModalShortcut::enabled(
            "Esc",
            modal::ModalAction::Close,
        )]),
    );
    frame.render_widget(
        Paragraph::new(service_performance_detail_lines(state, service.as_ref()))
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

fn render_wide_dashboard(frame: &mut Frame, area: Rect, state: &GameState) {
    let evaluation = evaluate_financial_recovery(state);

    // Company is an executive dashboard first and a ledger second. Keep the
    // most important financial signals above the operating footprint so a
    // player can understand the Company's state before scanning Journey detail.
    let shell = components::panel_block("Company", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let overview_height = match &evaluation {
        Ok(evaluation) if evaluation.status == FinancialStatus::Operating => 1,
        _ => 2,
    };
    let (attention, requires_attention) = attention_lines(state, &evaluation);
    let attention_height = if requires_attention {
        attention.len().saturating_add(1).min(4) as u16
    } else {
        1
    };

    if shell_inner.height >= 32 {
        let [overview_area, metrics_area, operations_area, services_area, attention_area, history_area] =
            Layout::vertical([
                Constraint::Length(overview_height),
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(attention_height),
                Constraint::Fill(1),
            ])
            .spacing(1)
            .areas(shell_inner);

        render_company_overview(frame, overview_area, state, &evaluation);
        render_key_metrics(frame, metrics_area, state);
        render_operating_summary(frame, operations_area, state);
        render_service_performance(frame, services_area, state);
        render_attention_panel(frame, attention_area, state, &evaluation);
        render_recent_activity(frame, history_area, state, true);
    } else {
        // Preserve a useful Recent Activity area on shorter terminals.
        let [overview_area, metrics_area, operations_area, attention_area, history_area] =
            Layout::vertical([
                Constraint::Length(overview_height),
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .spacing(1)
            .areas(shell_inner);

        render_company_overview(frame, overview_area, state, &evaluation);
        render_key_metrics(frame, metrics_area, state);
        render_operating_summary(frame, operations_area, state);
        render_attention_panel(frame, attention_area, state, &evaluation);
        render_recent_activity(frame, history_area, state, true);
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
        Ok(evaluation) if evaluation.status == FinancialStatus::Operating => vec![Line::from(vec![
            Span::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)).bold(),
            ),
            Span::styled(" · Working capital healthy", theme::secondary()),
        ])],
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
    let mut identity = vec![Line::from(vec![
        Span::styled(
            state.player_company.vehicle_keeper_mark.as_str().to_owned(),
            theme::primary_value().bold(),
        ),
        Span::styled(" · Rail ", theme::secondary()),
        Span::styled(
            format!("{} · {}", registration.display_code(), registration.mark),
            theme::primary_value(),
        ),
        Span::styled(" · ", theme::secondary()),
        Span::styled(state.region.name.clone(), theme::secondary()),
    ])];
    if area.height > 1 {
        identity.push(Line::styled(state.player_company.name.clone(), theme::hint()));
    }
    render_dashboard_section(frame, identity_area, identity);
}

fn render_key_metrics(frame: &mut Frame, area: Rect, state: &GameState) {
    let recent = RecentJourneyPerformance::from_state(state);
    let active = ActiveJourneyExposure::from_state(state);
    let [cash_area, result_area, revenue_area, costs_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    render_key_metric_card(
        frame,
        cash_area,
        "CASH",
        format_money(state.player_company.funds),
        "cash on hand".into(),
        format!(
            "{} pending revenue",
            format_cents(active.revenue_in_transit_cents)
        ),
        theme::primary_value(),
    );

    let (recent_result, recent_subtitle, recent_margin) = if recent.journey_count == 0 {
        ("—".into(), "no completed journeys".into(), "margin —".into())
    } else {
        (
            format_signed_cents(recent.result_cents),
            recent_window_label(recent.journey_count),
            format!(
                "{} margin",
                margin_label(recent.result_cents, recent.revenue_cents)
            ),
        )
    };
    render_key_metric_card(
        frame,
        result_area,
        "RECENT RESULT",
        recent_result,
        recent_subtitle,
        recent_margin,
        result_style(recent.result_cents),
    );

    let revenue_context = if recent.journey_count == 0 {
        "boardings —".into()
    } else {
        recent.passengers_carried.map_or_else(
            || "boardings unavailable".into(),
            |passengers| {
                format!(
                    "{passengers} {}",
                    if passengers == 1 { "boarding" } else { "boardings" }
                )
            },
        )
    };
    render_key_metric_card(
        frame,
        revenue_area,
        "RECENT REVENUE",
        if recent.journey_count == 0 {
            "—".into()
        } else {
            format_cents(recent.revenue_cents)
        },
        recent_window_label(recent.journey_count),
        revenue_context,
        theme::primary_value(),
    );

    render_key_metric_card(
        frame,
        costs_area,
        "RECENT COSTS",
        if recent.journey_count == 0 {
            "—".into()
        } else {
            format_cents(recent.operating_costs_cents)
        },
        recent_window_label(recent.journey_count),
        if recent.journey_count == 0 {
            "access + fuel".into()
        } else {
            format!(
                "{} avg / journey",
                format_cents(recent.operating_costs_cents / recent.journey_count as i128)
            )
        },
        theme::primary_value(),
    );
}

fn render_key_metric_card(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: String,
    subtitle: String,
    context: String,
    value_style: Style,
) {
    let block = components::panel_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(value, value_style.bold()),
            Line::styled(subtitle, theme::secondary()),
            Line::styled(context, theme::hint()),
        ])
        .alignment(Alignment::Center)
        .style(theme::panel()),
        inner,
    );
}

fn recent_window_label(journey_count: usize) -> String {
    match journey_count {
        0 => "no completed journeys".into(),
        1 => "last journey".into(),
        count => format!("last {count} journeys"),
    }
}

fn render_operating_summary(frame: &mut Frame, area: Rect, state: &GameState) {
    let [operations_area, trend_area, lifetime_area] = Layout::horizontal([
        Constraint::Fill(5),
        Constraint::Fill(5),
        Constraint::Length(24),
    ])
    .spacing(2)
    .areas(area);

    let trains = &state.player_company.fleet.trains;
    let running_services = running_service_count(state);
    let served_settlements = served_settlement_count(state);
    let connected_settlements = connected_settlement_count(state);
    let active = ActiveJourneyExposure::from_state(state);
    render_dashboard_section(
        frame,
        operations_area,
        vec![
            section_heading("OPERATIONS"),
            dashboard_line(
                "Fleet",
                format!(
                    "{} {} · {}",
                    trains.len(),
                    if trains.len() == 1 { "train" } else { "trains" },
                    format_cents(fleet_value_cents(state))
                ),
                theme::primary_value(),
            ),
            dashboard_line(
                "Services",
                service_operating_summary_label(state),
                if running_services > 0 {
                    theme::success()
                } else {
                    theme::primary_value()
                },
            ),
            dashboard_line(
                "Network",
                settlement_coverage_label(served_settlements, connected_settlements),
                theme::primary_value(),
            ),
            dashboard_line(
                "Running",
                format!(
                    "{} {} · {} aboard",
                    active.journey_count,
                    if active.journey_count == 1 {
                        "train"
                    } else {
                        "trains"
                    },
                    active.onboard_passengers
                ),
                theme::primary_value(),
            ),
        ],
    );

    render_recent_result_chart(frame, trend_area, state);

    let lifetime_result = operating_result_cents(state);
    render_dashboard_section(
        frame,
        lifetime_area,
        vec![
            section_heading("LIFETIME"),
            compact_dashboard_line(
                "Revenue",
                format_money(state.financials.operating_revenue),
                theme::primary_value(),
            ),
            compact_dashboard_line(
                "Costs",
                format_cents(operating_costs_cents(state)),
                theme::primary_value(),
            ),
            compact_dashboard_line(
                "Result",
                format_signed_cents(lifetime_result),
                result_style(lifetime_result),
            ),
            compact_dashboard_line(
                "Margin",
                operating_margin_label(state),
                result_style(lifetime_result),
            ),
        ],
    );
}

fn render_recent_result_chart(frame: &mut Frame, area: Rect, state: &GameState) {
    let recent = RecentJourneyPerformance::from_state(state);
    let [heading_area, chart_area, summary_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(section_heading("RECENT JOURNEY RESULTS · OLDEST → NEWEST"))
            .style(theme::panel()),
        heading_area,
    );

    if recent.journey_count == 0 {
        frame.render_widget(
            Paragraph::new(format!(
                "The latest {RECENT_JOURNEY_WINDOW} completed Journeys will appear here."
            ))
            .style(theme::secondary())
            .wrap(Wrap { trim: true }),
            chart_area,
        );
        frame.render_widget(
            Paragraph::new("No completed Journeys yet.").style(theme::hint()),
            summary_area,
        );
        return;
    }

    let points = recent_result_chart_points(state);
    let x_max = (points.len().saturating_sub(1).max(1)) as f64;
    let (y_min, y_max) = recent_result_chart_bounds(&points);
    let zero_line = [(0.0, 0.0), (x_max, 0.0)];
    let result_style = result_style(recent.result_cents);
    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(theme::secondary())
            .data(&zero_line),
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(result_style)
            .data(&points),
    ];
    let y_labels = recent_result_axis_labels(y_min, y_max);
    let chart = Chart::new(datasets)
        .style(theme::panel())
        .legend_position(None)
        .x_axis(
            Axis::default()
                .style(theme::secondary())
                .bounds([0.0, x_max]),
        )
        .y_axis(
            Axis::default()
                .style(theme::secondary())
                .bounds([y_min, y_max])
                .labels(y_labels),
        );
    frame.render_widget(chart, chart_area);

    let break_even = recent
        .journey_count
        .saturating_sub(recent.profitable_journeys + recent.loss_making_journeys);
    let mut summary = format!(
        "{} {} · {} profitable · {} {}",
        recent.journey_count,
        if recent.journey_count == 1 { "journey" } else { "journeys" },
        recent.profitable_journeys,
        recent.loss_making_journeys,
        if recent.loss_making_journeys == 1 {
            "loss"
        } else {
            "losses"
        }
    );
    if break_even > 0 {
        write!(summary, " · {break_even} break-even").ok();
    }
    frame.render_widget(
        Paragraph::new(summary)
            .alignment(Alignment::Center)
            .style(result_style),
        summary_area,
    );
}

fn recent_result_chart_points(state: &GameState) -> Vec<(f64, f64)> {
    let receipts = &state.financials.recent_journey_receipts;
    let start = receipts.len().saturating_sub(RECENT_JOURNEY_WINDOW);
    receipts[start..]
        .iter()
        .enumerate()
        .map(|(index, receipt)| {
            let result = receipt_result_cents(
                receipt.revenue,
                receipt.infrastructure_access_fee,
                receipt.fuel_cost,
            );
            (index as f64, result as f64)
        })
        .collect()
}

fn recent_result_chart_bounds(points: &[(f64, f64)]) -> (f64, f64) {
    let (mut minimum, mut maximum) = points.iter().fold((0.0_f64, 0.0_f64), |bounds, point| {
        (bounds.0.min(point.1), bounds.1.max(point.1))
    });

    if minimum == 0.0 && maximum == 0.0 {
        return (-100.0, 100.0);
    }

    let span = (maximum - minimum).max(100.0);
    let padding = span * 0.08;
    if minimum < 0.0 {
        minimum -= padding;
    }
    if maximum > 0.0 {
        maximum += padding;
    }
    (minimum, maximum)
}

fn recent_result_axis_labels(minimum: f64, maximum: f64) -> Vec<String> {
    let mut labels = vec![format_compact_chart_cents(minimum.round() as i128)];
    if minimum < 0.0 && maximum > 0.0 {
        labels.push("$0".into());
    }
    labels.push(format_compact_chart_cents(maximum.round() as i128));
    labels
}

fn format_compact_chart_cents(cents: i128) -> String {
    let sign = if cents < 0 { "−" } else { "" };
    let absolute_dollars = cents.abs() as f64 / 100.0;
    if absolute_dollars >= 1_000_000.0 {
        format!("{sign}${:.1}m", absolute_dollars / 1_000_000.0)
    } else if absolute_dollars >= 1_000.0 {
        format!("{sign}${:.1}k", absolute_dollars / 1_000.0)
    } else {
        format!("{sign}${:.0}", absolute_dollars)
    }
}

fn render_service_performance(frame: &mut Frame, area: Rect, state: &GameState) {
    let summary = ServicePerformanceSummary::from_state(state);
    let recent_journeys = summary.attributed_journeys + summary.unattributed_journeys;
    let [heading_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let visible_rows = usize::from(table_area.height.saturating_sub(2)).max(1);
    let shown_services = summary.services.len().min(visible_rows);
    let mut heading = if summary.attributed_journeys == 0 {
        if recent_journeys == 0 {
            "SERVICE PERFORMANCE · NO RECENT JOURNEYS".to_owned()
        } else {
            "SERVICE PERFORMANCE · WAITING FOR SERVICE DATA".to_owned()
        }
    } else {
        format!(
            "SERVICE PERFORMANCE · {} RECENT {}",
            summary.attributed_journeys,
            if summary.attributed_journeys == 1 {
                "JOURNEY"
            } else {
                "JOURNEYS"
            }
        )
    };
    if shown_services < summary.services.len() {
        heading.push_str(&format!(
            " · SHOWING {shown_services}/{} SERVICES",
            summary.services.len()
        ));
    }

    frame.render_widget(
        Paragraph::new(section_heading(&heading)).style(theme::panel()),
        heading_area,
    );

    if summary.services.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "No passenger Services are defined yet. Create a Service to begin operating the network.",
            )
            .style(theme::secondary())
            .wrap(Wrap { trim: true }),
            table_area,
        );
        return;
    }

    let show_positioning = summary
        .services
        .iter()
        .any(|service| service.positioning_journeys > 0);
    let full_width = if show_positioning { 118 } else { 107 };
    let full = table_area.width >= full_width;
    let rows = summary.services.iter().take(visible_rows).map(|service| {
        let has_recent_activity = service.journey_count() > 0;
        let boardings = if service.revenue_journeys == 0 {
            "—".into()
        } else {
            service
                .passengers_carried
                .map_or_else(|| "—".into(), |passengers| passengers.to_string())
        };
        let result = if has_recent_activity {
            Cell::from(format_signed_cents(service.result_cents))
                .style(result_style(service.result_cents))
        } else {
            Cell::from("—").style(theme::secondary())
        };
        let mut cells = vec![
            Cell::from(service_performance_label(
                state,
                service.service_id,
                &service.service_code,
            )),
            Cell::from(service_operating_status_label(state, service.service_id))
                .style(service_operating_status_style(state, service.service_id)),
            Cell::from(service.revenue_journeys.to_string()),
        ];
        if full {
            if show_positioning {
                cells.push(Cell::from(service.positioning_journeys.to_string()));
            }
            cells.extend([
                Cell::from(boardings),
                Cell::from(if has_recent_activity {
                    format_cents(service.revenue_cents)
                } else {
                    "—".into()
                }),
                Cell::from(if has_recent_activity {
                    format_cents(service.operating_costs_cents)
                } else {
                    "—".into()
                }),
                result,
            ]);
        } else {
            cells.extend([Cell::from(boardings), result]);
        }
        Row::new(cells)
    });

    let (header, widths, render_width) = if full && show_positioning {
        (
            Row::new([
                "Service",
                "Status",
                "Runs",
                "Positioning",
                "Boardings",
                "Revenue",
                "Costs",
                "Result",
            ]),
            vec![
                Constraint::Length(34),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(11),
                Constraint::Length(10),
                Constraint::Length(13),
                Constraint::Length(13),
                Constraint::Length(13),
            ],
            118,
        )
    } else if full {
        (
            Row::new([
                "Service",
                "Status",
                "Runs",
                "Boardings",
                "Revenue",
                "Costs",
                "Result",
            ]),
            vec![
                Constraint::Length(34),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(13),
                Constraint::Length(13),
                Constraint::Length(13),
            ],
            107,
        )
    } else {
        (
            Row::new(["Service", "Status", "Runs", "Boardings", "Result"]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(13),
            ],
            table_area.width,
        )
    };
    let table = Table::new(rows, widths)
        .header(header.style(theme::table_header()).bottom_margin(1))
        .style(theme::panel());
    let render_area = if full {
        Rect {
            width: table_area.width.min(render_width),
            ..table_area
        }
    } else {
        table_area
    };
    frame.render_widget(table, render_area);
}

fn service_performance_label(state: &GameState, service_id: ServiceId, service_code: &str) -> String {
    let Some(service) = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id)
    else {
        return format!("{service_code} · removed");
    };
    let (Some(origin), Some(destination)) = (
        service.origin_station_id(),
        service.destination_station_id(),
    ) else {
        return service_code.to_owned();
    };
    let separator = if service.direction_mode == ServiceDirectionMode::BothDirections {
        "↔"
    } else {
        "→"
    };
    format!(
        "{service_code} · {} {separator} {}",
        station_label(state, origin),
        station_label(state, destination)
    )
}

fn service_running_journey_count(state: &GameState, service_id: ServiceId) -> usize {
    state
        .active_journeys
        .iter()
        .filter(|journey| journey.service_id == service_id)
        .count()
}

fn service_assigned_train_count(state: &GameState, service_id: ServiceId) -> usize {
    state
        .player_company
        .fleet
        .service_assignments
        .values()
        .filter(|&&assigned_service_id| assigned_service_id == service_id)
        .count()
}

fn service_operating_status_label(state: &GameState, service_id: ServiceId) -> &'static str {
    let is_current = state
        .player_company
        .passenger_services
        .iter()
        .any(|service| service.id == service_id);
    if !is_current {
        return "REMOVED";
    }
    if service_running_journey_count(state, service_id) > 0 {
        "RUNNING"
    } else if service_assigned_train_count(state, service_id) > 0 {
        "ASSIGNED"
    } else {
        "UNASSIGNED"
    }
}

fn service_operating_status_style(state: &GameState, service_id: ServiceId) -> Style {
    match service_operating_status_label(state, service_id) {
        "RUNNING" => theme::success(),
        "UNASSIGNED" => theme::warning(),
        "REMOVED" => theme::secondary(),
        _ => theme::primary_value(),
    }
}

fn service_operating_summary_label(state: &GameState) -> String {
    let mut running = 0;
    let mut assigned = 0;
    let mut unassigned = 0;

    for service in &state.player_company.passenger_services {
        if service_running_journey_count(state, service.id) > 0 {
            running += 1;
        } else if service_assigned_train_count(state, service.id) > 0 {
            assigned += 1;
        } else {
            unassigned += 1;
        }
    }

    let mut parts = vec![format!("{running} running")];
    if assigned > 0 {
        parts.push(format!("{assigned} assigned"));
    }
    if unassigned > 0 {
        parts.push(format!("{unassigned} unassigned"));
    }
    parts.join(" · ")
}

fn running_service_count(state: &GameState) -> usize {
    state
        .player_company
        .passenger_services
        .iter()
        .filter(|service| service_running_journey_count(state, service.id) > 0)
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

fn render_compact_dashboard(frame: &mut Frame, area: Rect, state: &GameState) {
    let evaluation = evaluate_financial_recovery(state);
    let shell = components::panel_block("Company", true);
    let shell_inner = shell.inner(area);
    frame.render_widget(shell, area);

    let [summary_area, right_area] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Fill(1)])
            .spacing(2)
            .areas(shell_inner);
    let (_, requires_attention) = attention_lines(state, &evaluation);
    let attention_height = if requires_attention {
        if right_area.height >= 17 { 7 } else { 5 }
    } else {
        2
    };
    let [attention_area, history_area] = Layout::vertical([
        Constraint::Length(attention_height),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(right_area);

    render_compact_summary(frame, summary_area, state, &evaluation);
    render_attention_panel(frame, attention_area, state, &evaluation);
    render_recent_activity(frame, history_area, state, false);
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
    let recent = RecentJourneyPerformance::from_state(state);
    let mut lines = vec![
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
    if inner.height >= 12 {
        lines.extend([
            Line::from(""),
            section_heading("RECENT"),
            dashboard_line(
                "Completed",
                recent.journey_count.to_string(),
                theme::primary_value(),
            ),
            dashboard_line(
                "Result",
                if recent.journey_count == 0 {
                    "—".into()
                } else {
                    format_signed_cents(recent.result_cents)
                },
                result_style(recent.result_cents),
            ),
        ]);
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_recent_activity(frame: &mut Frame, area: Rect, state: &GameState, wide: bool) {
    let receipts = &state.financials.recent_journey_receipts;
    let shown = receipts.len().min(RECENT_ACTIVITY_LIMIT);
    let heading = if shown == 0 {
        "RECENT ACTIVITY".to_owned()
    } else {
        format!("RECENT ACTIVITY · LAST {shown}")
    };
    let [heading_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading(&heading)).style(theme::panel()),
        heading_area,
    );

    if receipts.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "No completed Journey activity yet. Journey receipts will appear here after Services reach their termini; press H for full history.",
            )
            .style(theme::secondary())
            .wrap(Wrap { trim: true }),
            content_area,
        );
        return;
    }

    let detailed = wide && content_area.width >= 110;
    let rows = receipts.iter().rev().take(RECENT_ACTIVITY_LIMIT).map(|receipt| {
        let result = receipt_result_cents(
            receipt.revenue,
            receipt.infrastructure_access_fee,
            receipt.fuel_cost,
        );
        if detailed {
            Row::new([
                Cell::from(receipt_age_label(state, receipt)),
                Cell::from(receipt_service_label(receipt)),
                Cell::from(receipt_route_label(state, receipt)),
                Cell::from(receipt_train_label(receipt)),
                Cell::from(receipt_boardings_label(receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        } else {
            Row::new([
                Cell::from(receipt_route_or_id_label(state, receipt)),
                Cell::from(receipt_boardings_label(receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        }
    });
    let (header, widths) = if detailed {
        (
            Row::new([
                "Completed",
                "Service",
                "Route",
                "Train",
                "Boardings",
                "Result",
            ]),
            vec![
                Constraint::Length(11),
                Constraint::Length(9),
                Constraint::Length(32),
                Constraint::Length(26),
                Constraint::Length(10),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(["Journey", "Boardings", "Result"]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(header.style(theme::table_header()).bottom_margin(1))
        .style(theme::panel());
    let table_area = if detailed {
        Rect {
            width: content_area.width.min(110),
            ..content_area
        }
    } else {
        content_area
    };
    frame.render_widget(table, table_area);
}

fn render_receipt_history_table(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    wide: bool,
) {
    let receipts = &state.financials.recent_journey_receipts;
    if receipts.is_empty() {
        frame.render_widget(
            Paragraph::new("No retained Journey receipts yet. Operating Revenue is credited as passengers reach their stops; a Journey receipt is retained at the Service terminus.")
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let visible_items = usize::from(area.height.saturating_sub(2)).max(1);
    selection.set_page_size(visible_items);
    let detailed = wide && area.width >= 82;
    let rows = receipts.iter().rev().map(|receipt| {
        let result = receipt_result_cents(
            receipt.revenue,
            receipt.infrastructure_access_fee,
            receipt.fuel_cost,
        );
        if detailed {
            Row::new([
                Cell::from(journey_reference_label(receipt.journey_id)),
                Cell::from(receipt_route_label(state, receipt)),
                Cell::from(receipt_train_label(receipt)),
                Cell::from(receipt_boardings_label(receipt)),
                Cell::from(receipt_capacity_label(receipt)),
                Cell::from(receipt_age_label(state, receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        } else {
            Row::new([
                Cell::from(receipt_route_or_id_label(state, receipt)),
                Cell::from(receipt_boardings_label(receipt)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        }
    });
    let (header, widths) = if detailed {
        (
            Row::new([
                "ID",
                "Route",
                "Train",
                "Boardings",
                "Capacity",
                "Completed",
                "Result",
            ]),
            vec![
                Constraint::Length(9),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(11),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(["Journey", "Boardings", "Result"]),
            vec![
                Constraint::Fill(1),
                Constraint::Length(10),
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
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

fn attention_lines(
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) -> (Vec<Line<'static>>, bool) {
    let recent = RecentJourneyPerformance::from_state(state);
    let services = ServicePerformanceSummary::from_state(state);
    let mut lines = Vec::new();
    let mut has_warning = false;

    match evaluation {
        Ok(evaluation) if evaluation.status != FinancialStatus::Operating => {
            has_warning = true;
            lines.push(Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            ));
            lines.push(Line::styled(
                status_explanation(evaluation.status),
                theme::secondary(),
            ));
            if evaluation.status == FinancialStatus::Insolvent
                && !evaluation.recovery_options.is_empty()
            {
                lines.push(Line::styled(
                    format!(
                        "Concrete recovery options: {} {} · [R] review.",
                        evaluation.recovery_options.len(),
                        if evaluation.recovery_options.len() == 1 {
                            "route"
                        } else {
                            "routes"
                        }
                    ),
                    theme::warning(),
                ));
            }
        }
        Err(error) => {
            has_warning = true;
            lines.push(Line::styled(
                format!("[?] Financial status unavailable: {error}"),
                theme::error(),
            ));
        }
        Ok(_) => {}
    }

    for service in state
        .player_company
        .passenger_services
        .iter()
        .filter(|service| {
            service_running_journey_count(state, service.id) == 0
                && service_assigned_train_count(state, service.id) == 0
        })
        .take(ATTENTION_SERVICE_LIMIT)
    {
        has_warning = true;
        lines.push(Line::styled(
            format!("[!] {} has no Train assigned.", service.name),
            theme::warning(),
        ));
    }

    if recent.journey_count == 0 {
        if state.player_company.passenger_services.is_empty() {
            lines.push(Line::styled(
                "[~] No passenger Services are defined yet.",
                theme::hint(),
            ));
        } else {
            lines.push(Line::styled(
                "[~] No completed Journey data yet.",
                theme::hint(),
            ));
        }
    } else if recent.loss_making_journeys > 0 {
        has_warning = true;
        lines.push(Line::styled(
            format!(
                "[!] {} of the last {} completed {} lost money.",
                recent.loss_making_journeys,
                recent.journey_count,
                if recent.journey_count == 1 {
                    "Journey"
                } else {
                    "Journeys"
                }
            ),
            theme::warning(),
        ));
    }

    for service in services
        .services
        .iter()
        .filter(|service| service.journey_count() > 0 && service.result_cents < 0)
        .take(ATTENTION_SERVICE_LIMIT)
    {
        has_warning = true;
        let journeys = service.journey_count();
        lines.push(Line::styled(
            format!(
                "[!] {}: {} across {} recent {}.",
                service.service_code,
                format_signed_cents(service.result_cents),
                journeys,
                if journeys == 1 { "Journey" } else { "Journeys" }
            ),
            theme::warning(),
        ));
    }

    if !has_warning && recent.journey_count > 0 {
        lines.push(Line::styled(
            "[OK] No issues requiring attention.",
            theme::success(),
        ));
    }

    (lines, has_warning)
}

fn render_attention_panel(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let (lines, _) = attention_lines(state, evaluation);
    if area.height <= 1 {
        let mut spans = vec![Span::styled("ATTENTION  ", theme::secondary().bold())];
        if let Some(line) = lines.into_iter().next() {
            spans.extend(line.spans);
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(theme::panel()),
            area,
        );
        return;
    }

    let [heading_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(section_heading("ATTENTION")).style(theme::panel()),
        heading_area,
    );
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
                "{} defined · {} running",
                state.player_company.passenger_services.len(),
                running_service_count(state),
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

fn service_performance_detail_lines(
    state: &GameState,
    service: Option<&ServicePerformance>,
) -> Vec<Line<'static>> {
    let Some(service) = service else {
        return vec![Line::styled(
            "The selected Service is no longer present in the recent performance window.",
            theme::secondary(),
        )];
    };

    let journey_count = service.journey_count();
    let average_result = if journey_count == 0 {
        None
    } else {
        Some(service.result_cents / i128::try_from(journey_count).unwrap_or(1))
    };
    let passengers = service
        .passengers_carried
        .map_or_else(|| "—".into(), |passengers| passengers.to_string());

    vec![
        Line::styled(
            service_performance_label(state, service.service_id, &service.service_code),
            theme::title(),
        ),
        Line::styled(
            format!(
                "Calculated from this Service's Journeys inside the latest {RECENT_JOURNEY_WINDOW} completed-Journey window."
            ),
            theme::secondary(),
        ),
        Line::from(""),
        section_heading("OPERATIONS"),
        financial_line(
            "Status",
            service_operating_status_label(state, service.service_id).to_owned(),
            service_operating_status_style(state, service.service_id),
        ),
        financial_line(
            "Revenue runs",
            service.revenue_journeys.to_string(),
            theme::primary_value(),
        ),
        financial_line(
            "Positioning moves",
            service.positioning_journeys.to_string(),
            theme::primary_value(),
        ),
        financial_line("Boardings", passengers, theme::primary_value()),
        Line::from(""),
        section_heading("FINANCIAL"),
        financial_line(
            "Revenue",
            format_cents(service.revenue_cents),
            theme::primary_value(),
        ),
        financial_line(
            "Access fees",
            format_cents(service.access_fees_cents),
            theme::primary_value(),
        ),
        financial_line(
            "Fuel",
            format_cents(service.fuel_costs_cents),
            theme::primary_value(),
        ),
        financial_line(
            "Operating costs",
            format_cents(service.operating_costs_cents),
            theme::primary_value(),
        ),
        financial_line(
            "Result",
            format_signed_cents(service.result_cents),
            result_style(service.result_cents),
        ),
        financial_line(
            "Margin",
            margin_label(service.result_cents, service.revenue_cents),
            result_style(service.result_cents),
        ),
        Line::from(""),
        section_heading("JOURNEY RESULTS"),
        financial_line(
            "Best Journey",
            service
                .best_result_cents
                .map_or_else(|| "—".into(), format_signed_cents),
            result_style(service.best_result_cents.unwrap_or_default()),
        ),
        financial_line(
            "Worst Journey",
            service
                .worst_result_cents
                .map_or_else(|| "—".into(), format_signed_cents),
            result_style(service.worst_result_cents.unwrap_or_default()),
        ),
        financial_line(
            "Average / Journey",
            average_result.map_or_else(|| "—".into(), format_signed_cents),
            result_style(average_result.unwrap_or_default()),
        ),
    ]
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
                        "{} · {}",
                        receipt_route_label(state, receipt),
                        receipt_age_label(state, receipt)
                    ),
                    theme::title(),
                ),
                Line::from(""),
            ];

            if receipt.service_code.is_some() || receipt.purpose.is_some() {
                lines.push(section_heading("SERVICE"));
                lines.extend([
                    financial_line(
                        "Service",
                        receipt.service_code.clone().unwrap_or_else(|| "—".into()),
                        theme::primary_value(),
                    ),
                    financial_line(
                        "Purpose",
                        receipt
                            .purpose
                            .map_or_else(|| "—".into(), journey_purpose_label),
                        theme::primary_value(),
                    ),
                    financial_line(
                        "Journey time",
                        receipt_journey_time_label(receipt),
                        theme::primary_value(),
                    ),
                    Line::from(""),
                ]);
            }

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
                        "Boardings",
                        receipt_boardings_label(receipt),
                        theme::primary_value(),
                    ),
                    financial_line(
                        "Capacity",
                        receipt_capacity_label(receipt),
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

fn journey_purpose_label(purpose: JourneyPurpose) -> String {
    match purpose {
        JourneyPurpose::RevenueService => "Revenue service".into(),
        JourneyPurpose::Positioning => "Positioning".into(),
    }
}

fn receipt_journey_time_label(receipt: &JourneyReceipt) -> String {
    let (Some(departed_at), Some(completed_at)) = (receipt.departed_at, receipt.completed_at) else {
        return "—".into();
    };
    let seconds = completed_at
        .unix_seconds()
        .saturating_sub(departed_at.unix_seconds())
        .max(0) as u64;
    duration_label(seconds)
}

fn duration_label(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60),
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

fn compact_dashboard_line(label: &str, value: String, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<8}"), theme::secondary()),
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
    margin_label(
        operating_result_cents(state),
        i128::from(state.financials.operating_revenue.cents()),
    )
}

fn margin_label(result_cents: i128, revenue_cents: i128) -> String {
    if revenue_cents == 0 {
        return "—".into();
    }

    let tenths = result_cents.saturating_mul(1_000) / revenue_cents;
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
            format!(
                "{} boarded · {}",
                receipt_boardings_label(receipt),
                receipt_capacity_label(receipt)
            ),
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
        journey_reference_label(receipt.journey_id)
    }
}

fn journey_reference_label(journey_id: JourneyId) -> String {
    const DISPLAY_MODULUS: u64 = 100_000_000;

    let suffix = journey_id.get();
    if suffix < 100 {
        format!("J{suffix:02}")
    } else if suffix < DISPLAY_MODULUS {
        format!("J{suffix}")
    } else {
        format!("J{:08}", suffix % DISPLAY_MODULUS)
    }
}

fn receipt_service_label(receipt: &JourneyReceipt) -> String {
    receipt
        .service_code
        .clone()
        .unwrap_or_else(|| "—".into())
}

fn receipt_train_label(receipt: &JourneyReceipt) -> String {
    match (receipt.train_id, receipt.train_model_name.as_deref()) {
        (Some(train_id), Some(model_name)) => format!("T{} · {model_name}", train_id.get()),
        (Some(train_id), None) => format!("Train {}", train_id.get()),
        (None, Some(model_name)) => model_name.to_owned(),
        (None, None) => "—".into(),
    }
}

fn receipt_boardings_label(receipt: &JourneyReceipt) -> String {
    receipt
        .passengers_carried
        .map_or_else(|| "—".into(), |passengers| passengers.to_string())
}

fn receipt_capacity_label(receipt: &JourneyReceipt) -> String {
    receipt
        .passenger_capacity
        .map_or_else(|| "—".into(), |capacity| format!("{capacity} seats"))
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
        assert_eq!(shortcuts[0].key, "Esc");
        assert_eq!(shortcuts[0].action, "Cancel");
        assert_eq!(shortcuts[1].key, "Enter");
        assert_eq!(shortcuts[1].action, "Save");
        assert!(workspace.help_lines(&state)[0].contains("Company VKM"));
    }

    #[test]
    fn company_workspace_drills_into_recent_service_performance() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).unwrap();

        let mut workspace = CompanyWorkspace::default();
        assert_eq!(
            workspace.handle_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &state,
            ),
            super::CompanyWorkspaceAction::ClearNotice
        );
        assert!(workspace.service_performance_browser_open());
        assert!(workspace.help_lines(&state)[0].contains("Service Performance"));
        assert_eq!(workspace.shortcuts(&state, false, true)[0].key, "Esc");

        assert_eq!(
            workspace.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            super::CompanyWorkspaceAction::ClearNotice
        );
        assert!(workspace.service_performance_details_open);

        workspace.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(workspace.service_performance_open);
        assert!(!workspace.service_performance_details_open);
        workspace.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(!workspace.service_performance_browser_open());
    }
}
