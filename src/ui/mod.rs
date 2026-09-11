//! Terminal shell and shared navigation state.
//!
//! This module deliberately owns terminal I/O only. Game actions remain at the
//! application boundary, where callers supply an elapsed-time reconciliation
//! callback before the shell accepts each input event.

use std::{
    error::Error,
    fmt,
    io::{self, Stdout},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Layout, Rect},
    style::Color,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Tabs, Wrap},
};

use crate::{
    APPLICATION_NAME,
    model::{GameState, Money, RailStationId, TrainId, TrainStatus, UtcSeconds},
    sim::finance::{FinancialStatus, evaluate_financial_recovery},
};

pub mod company;
pub mod dispatch;
pub mod fleet;
pub mod format;
pub mod map;
pub mod market;
pub mod start;
pub mod theme;

/// How frequently the shell checks for elapsed arrivals while no key is pressed.
pub const ARRIVAL_POLL_INTERVAL: Duration = Duration::from_millis(250);

const MINIMUM_COLUMNS: u16 = 64;
const MINIMUM_ROWS: u16 = 16;

/// The four primary views in the RailQ terminal shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum View {
    /// The Rail Network overview and operational home.
    #[default]
    Map,
    /// The Player Company's Fleet.
    Trains,
    /// The Player Company's financial position.
    Company,
    /// The Train catalogue and purchase flow.
    BuyTrains,
}

impl View {
    const fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Trains => "Fleet",
            Self::Company => "Company",
            Self::BuyTrains => "Buy Trains",
        }
    }

    const fn shortcut(self) -> char {
        match self {
            Self::Map => 'M',
            Self::Trains => 'T',
            Self::Company => 'C',
            Self::BuyTrains => 'B',
        }
    }
}

/// The outcome of a keyboard event handled by the shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellAction {
    /// Continue running; the active view may have changed.
    Continue,
    /// Exit the terminal shell.
    Exit,
    /// Confirmed player input requiring an application-boundary Manual Dispatch.
    ManualDispatch {
        train_id: TrainId,
        destination_station_id: RailStationId,
    },
    /// Confirmed player input requiring an application-boundary Train purchase.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
    /// Confirmed player input requiring an application-boundary Train resale.
    SellTrain { train_id: TrainId },
    /// Confirmed Bankruptcy restart requiring an archived-save application action.
    RestartAfterBankruptcy,
}

/// One command accepted by the terminal shell at the application boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalCommand {
    /// Reconcile elapsed demand and due Journey arrivals before presentation or input.
    Reconcile { now: UtcSeconds },
    /// Revalidate and authorise a player-requested Manual Dispatch.
    ManualDispatch {
        train_id: TrainId,
        destination_station_id: RailStationId,
        now: UtcSeconds,
    },
    /// Revalidate and purchase a selected catalogue Train.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
        now: UtcSeconds,
    },
    /// Revalidate and resell a selected READY Train.
    SellTrain { train_id: TrainId, now: UtcSeconds },
    /// Archive the Bankrupt Player Company save and start a fresh game.
    RestartAfterBankruptcy { world_seed: u64, now: UtcSeconds },
}

/// Presentation-only record of a command which crossed the save boundary.
/// It is deliberately not saved: reopening a game must not invent old notices.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ActionOutcome {
    title: &'static str,
    summary: String,
    details: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingAction {
    label: String,
    details: Vec<String>,
    funds_before: Money,
}

/// The active panel within the Fleet workspace. Compact terminals expose one
/// panel at a time; a wide workspace keeps both panels visible.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum FleetFocus {
    #[default]
    List,
    Details,
}

/// Presentation-only state shared by the four primary views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    active_view: View,
    dispatch_flow: Option<dispatch::DispatchFlow>,
    dispatch_returns_to_fleet: bool,
    map_selection: map::StationSelection,
    map_settlement_selection: map::SettlementSelection,
    map_journey_selection: map::JourneySelection,
    map_focus: map::MapFocus,
    map_details_open: bool,
    map_split_visible: bool,
    fleet_flow: Option<fleet::FleetFlow>,
    fleet_selection: fleet::FleetSelection,
    fleet_details_open: bool,
    fleet_focus: FleetFocus,
    fleet_split_visible: bool,
    market_flow: Option<market::MarketFlow>,
    market_selection: market::CatalogueSelection,
    company_receipt_selection: company::ReceiptSelection,
    company_receipt_details_open: bool,
    company_recovery_selection: company::RecoverySelection,
    company_recovery_review_open: bool,
    notice: Option<String>,
    pending_action: Option<PendingAction>,
    action_outcome: Option<ActionOutcome>,
    outcome_details_open: bool,
    help_visible: bool,
    help_offset: usize,
    restart_confirmation: bool,
}

impl Shell {
    /// Creates a shell with Map as the primary view.
    pub fn new() -> Self {
        Self {
            active_view: View::Map,
            dispatch_flow: None,
            dispatch_returns_to_fleet: false,
            map_selection: map::StationSelection::default(),
            map_settlement_selection: map::SettlementSelection::default(),
            map_journey_selection: map::JourneySelection::default(),
            map_focus: map::MapFocus::default(),
            map_details_open: false,
            map_split_visible: false,
            fleet_flow: None,
            fleet_selection: fleet::FleetSelection::default(),
            fleet_details_open: false,
            fleet_focus: FleetFocus::List,
            fleet_split_visible: false,
            market_flow: None,
            market_selection: market::CatalogueSelection::default(),
            company_receipt_selection: company::ReceiptSelection::default(),
            company_receipt_details_open: false,
            company_recovery_selection: company::RecoverySelection::default(),
            company_recovery_review_open: false,
            notice: None,
            pending_action: None,
            action_outcome: None,
            outcome_details_open: false,
            help_visible: false,
            help_offset: 0,
            restart_confirmation: false,
        }
    }

    /// Returns the selected primary view.
    pub fn active_view(&self) -> View {
        self.active_view
    }

    /// Returns whether the keyboard help overlay is currently visible.
    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    /// Routes global navigation and exit keys.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        if key.kind != KeyEventKind::Press {
            return ShellAction::Continue;
        }

        if matches!(key.code, KeyCode::Char('q' | 'Q'))
            || (matches!(key.code, KeyCode::Char('c' | 'C'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return ShellAction::Exit;
        }

        if self.help_visible {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'h' | 'H') => {
                    self.help_visible = false;
                    self.help_offset = 0;
                }
                KeyCode::Up | KeyCode::Char('k' | 'K') => {
                    self.help_offset = self.help_offset.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j' | 'J') => {
                    self.help_offset = self
                        .help_offset
                        .saturating_add(1)
                        .min(HELP_LINES.len().saturating_sub(1));
                }
                KeyCode::PageUp => {
                    self.help_offset = self.help_offset.saturating_sub(HELP_PAGE_STEP);
                }
                KeyCode::PageDown => {
                    self.help_offset = self
                        .help_offset
                        .saturating_add(HELP_PAGE_STEP)
                        .min(HELP_LINES.len().saturating_sub(1));
                }
                _ => {}
            }
            return ShellAction::Continue;
        }

        if self.outcome_details_open {
            match key.code {
                KeyCode::Esc => self.outcome_details_open = false,
                KeyCode::Enter | KeyCode::Char('a' | 'A') => {
                    self.outcome_details_open = false;
                    self.action_outcome = None;
                }
                _ => {}
            }
            return ShellAction::Continue;
        }

        if self.action_outcome.is_some() {
            match key.code {
                KeyCode::Enter => self.outcome_details_open = true,
                KeyCode::Esc | KeyCode::Char('a' | 'A') => self.action_outcome = None,
                _ => {}
            }
            return ShellAction::Continue;
        }

        if matches!(key.code, KeyCode::Char('?' | 'h' | 'H')) {
            self.help_visible = true;
            self.help_offset = 0;
            return ShellAction::Continue;
        }

        if is_bankrupt(state) {
            if self.restart_confirmation {
                return match key.code {
                    KeyCode::Enter => ShellAction::RestartAfterBankruptcy,
                    KeyCode::Esc => {
                        self.restart_confirmation = false;
                        self.notice = Some("Safe restart cancelled; the Bankrupt Player Company save is unchanged.".into());
                        ShellAction::Continue
                    }
                    _ => ShellAction::Continue,
                };
            }
            if matches!(key.code, KeyCode::Char('r' | 'R')) {
                self.restart_confirmation = true;
                self.notice = None;
            } else {
                self.notice = Some(
                    "Bankruptcy prevents normal operations. Press R for a safe restart or Q to exit."
                        .into(),
                );
            }
            return ShellAction::Continue;
        }

        self.restart_confirmation = false;

        if let Some(flow) = &mut self.dispatch_flow {
            return match flow.handle_key(key, state) {
                dispatch::DispatchFlowAction::Continue => ShellAction::Continue,
                dispatch::DispatchFlowAction::Cancel => {
                    self.dispatch_flow = None;
                    if self.dispatch_returns_to_fleet {
                        self.fleet_details_open = false;
                        self.fleet_focus = FleetFocus::List;
                    }
                    self.dispatch_returns_to_fleet = false;
                    self.notice = Some("Manual Dispatch cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                dispatch::DispatchFlowAction::Confirm {
                    train_id,
                    destination_station_id,
                } => {
                    self.pending_action =
                        Some(pending_dispatch(state, train_id, destination_station_id));
                    ShellAction::ManualDispatch {
                        train_id,
                        destination_station_id,
                    }
                }
            };
        }

        if let Some(flow) = &mut self.fleet_flow {
            return match flow.handle_key(key, state) {
                fleet::FleetFlowAction::Continue => ShellAction::Continue,
                fleet::FleetFlowAction::Cancel => {
                    self.fleet_flow = None;
                    self.notice = Some("Train resale cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                fleet::FleetFlowAction::Confirm { train_id } => {
                    self.pending_action = Some(pending_resale(state, train_id));
                    ShellAction::SellTrain { train_id }
                }
            };
        }

        if let Some(flow) = &mut self.market_flow {
            return match flow.handle_key(key, state) {
                market::MarketFlowAction::Continue => ShellAction::Continue,
                market::MarketFlowAction::Cancel => {
                    self.market_flow = None;
                    self.notice = Some("Train purchase cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                market::MarketFlowAction::ReturnToCatalogue => {
                    self.market_flow = None;
                    self.notice = None;
                    ShellAction::Continue
                }
                market::MarketFlowAction::ReturnToDelivery => ShellAction::Continue,
                market::MarketFlowAction::Confirm {
                    catalogue_index,
                    delivery_station_id,
                } => {
                    self.pending_action = Some(pending_purchase(
                        state,
                        catalogue_index,
                        delivery_station_id,
                    ));
                    ShellAction::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                    }
                }
            };
        }

        if self.company_recovery_review_open {
            match key.code {
                KeyCode::Esc => {
                    self.company_recovery_review_open = false;
                    self.notice = Some("Recovery review closed; no changes were made.".into());
                }
                KeyCode::Enter => {
                    let Some(destination) =
                        self.company_recovery_selection.selected_destination(state)
                    else {
                        self.company_recovery_review_open = false;
                        self.notice = Some(
                            "Recovery route changed; review the current Company status again."
                                .into(),
                        );
                        return ShellAction::Continue;
                    };
                    self.company_recovery_review_open = false;
                    self.active_view = match destination {
                        company::RecoveryDestination::Fleet => View::Trains,
                        company::RecoveryDestination::BuyTrains => View::BuyTrains,
                        company::RecoveryDestination::Map => View::Map,
                    };
                    self.notice = Some(
                        "Recovery route opened for review only; no action has been authorised."
                            .into(),
                    );
                }
                KeyCode::Char('m' | 'M') => {
                    self.company_recovery_review_open = false;
                    self.active_view = View::Map;
                }
                KeyCode::Char('t' | 'T') => {
                    self.company_recovery_review_open = false;
                    self.active_view = View::Trains;
                }
                KeyCode::Char('b' | 'B') => {
                    self.company_recovery_review_open = false;
                    self.active_view = View::BuyTrains;
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                    self.company_recovery_selection.handle_key(key.code, state);
                }
                _ => {}
            }
            return ShellAction::Continue;
        }

        match key.code {
            KeyCode::Char('m' | 'M') => self.active_view = View::Map,
            KeyCode::Char('t' | 'T') => {
                self.active_view = View::Trains;
                self.fleet_details_open = false;
                self.fleet_focus = FleetFocus::List;
                self.fleet_split_visible = false;
            }
            KeyCode::Char('c' | 'C') => {
                self.active_view = View::Company;
                self.company_receipt_details_open = false;
                self.company_recovery_review_open = false;
            }
            KeyCode::Char('r' | 'R') if self.active_view == View::Company => {
                if self
                    .company_recovery_selection
                    .selected_destination(state)
                    .is_some()
                {
                    self.company_recovery_review_open = true;
                    self.company_receipt_details_open = false;
                    self.notice = None;
                } else {
                    self.notice = Some(
                        "No finite recovery route is available while the Player Company is operating."
                            .into(),
                    );
                }
            }
            KeyCode::Char('b' | 'B') => self.active_view = View::BuyTrains,
            KeyCode::Enter if self.active_view == View::Map => {
                let has_selection = if self.map_focus.is_stations() {
                    self.map_selection.selected_station_id(state).is_some()
                } else if self.map_focus == map::MapFocus::Settlements {
                    self.map_settlement_selection
                        .selected_settlement_id(state)
                        .is_some()
                } else {
                    self.map_journey_selection
                        .selected_journey_id(state)
                        .is_some()
                };
                if has_selection {
                    self.map_details_open = true;
                    self.notice = None;
                }
            }
            KeyCode::Esc if self.active_view == View::Map && self.map_details_open => {
                self.map_details_open = false;
            }
            KeyCode::Enter if self.active_view == View::Company => {
                if self.company_receipt_selection.has_selection(state) {
                    self.company_receipt_details_open = true;
                    self.notice = None;
                }
            }
            KeyCode::Esc
                if self.active_view == View::Company && self.company_receipt_details_open =>
            {
                self.company_receipt_details_open = false;
            }
            KeyCode::Enter if self.active_view == View::BuyTrains => {
                match self.market_selection.selected_catalogue_index(state) {
                    Some(catalogue_index) => {
                        match market::MarketFlow::start(state, catalogue_index) {
                            Ok(flow) => {
                                self.market_flow = Some(flow);
                                self.notice = None;
                            }
                            Err(message) => self.notice = Some(message.into()),
                        }
                    }
                    None => {
                        self.notice = Some("No diesel Train is available in the catalogue.".into())
                    }
                }
            }
            KeyCode::Enter if self.active_view == View::Trains => {
                if self.fleet_selection.selected_train_id(state).is_some() {
                    self.fleet_details_open = true;
                    self.fleet_focus = FleetFocus::Details;
                    self.notice = None;
                }
            }
            KeyCode::Esc if self.active_view == View::Trains && self.fleet_details_open => {
                self.fleet_details_open = false;
                self.fleet_focus = FleetFocus::List;
            }
            KeyCode::Char('s' | 'S') if self.active_view == View::Trains => {
                match self.fleet_selection.selected_train_id(state) {
                    Some(train_id) => match fleet::FleetFlow::start(state, train_id) {
                        Ok(flow) => {
                            self.fleet_flow = Some(flow);
                            self.notice = None;
                        }
                        Err(message) => self.notice = Some(message),
                    },
                    None => {
                        self.notice =
                            Some("Select a Train before starting a resale review.".into());
                    }
                }
            }
            KeyCode::Char('d' | 'D') if self.active_view == View::Trains => {
                match self.fleet_selection.selected_train_id(state) {
                    Some(train_id) => {
                        match dispatch::DispatchFlow::start_for_train(state, train_id) {
                            Ok(flow) => {
                                self.dispatch_flow = Some(flow);
                                self.dispatch_returns_to_fleet = true;
                                self.notice = None;
                            }
                            Err(message) => self.notice = Some(message),
                        }
                    }
                    None => {
                        self.notice =
                            Some("Select a READY Train before starting Manual Dispatch.".into());
                    }
                }
            }
            KeyCode::Tab | KeyCode::BackTab
                if self.active_view == View::Trains && self.fleet_split_visible =>
            {
                self.fleet_focus = match self.fleet_focus {
                    FleetFocus::List => {
                        self.fleet_details_open = true;
                        FleetFocus::Details
                    }
                    FleetFocus::Details => FleetFocus::List,
                };
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::BuyTrains =>
            {
                self.market_selection.handle_key(key.code, state);
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Trains =>
            {
                self.fleet_selection.handle_key(key.code, state);
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Map
                    && (!self.map_details_open || self.map_split_visible) =>
            {
                if self.map_focus.is_stations() {
                    self.map_selection.handle_key(key.code, state);
                } else if self.map_focus == map::MapFocus::Settlements {
                    self.map_settlement_selection.handle_key(key.code, state);
                } else {
                    self.map_journey_selection.handle_key(key.code, state);
                    if let Some(train_id) = self.map_journey_selection.selected_train_id(state) {
                        self.fleet_selection.select_train_id(state, train_id);
                    }
                }
            }
            KeyCode::Tab | KeyCode::BackTab
                if self.active_view == View::Map && self.dispatch_flow.is_none() =>
            {
                self.map_focus = if self.map_focus.is_stations() {
                    map::MapFocus::Settlements
                } else if self.map_focus == map::MapFocus::Settlements {
                    map::MapFocus::Journeys
                } else {
                    map::MapFocus::Stations
                };
                self.map_details_open = false;
                self.notice = None;
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Company && !self.company_receipt_details_open =>
            {
                self.company_receipt_selection.handle_key(key.code, state);
            }
            KeyCode::Char('d' | 'D') if self.active_view == View::Map => {
                if self.map_focus.is_stations() {
                    let preferred_station_id = self.map_selection.selected_station_id(state);
                    match dispatch::DispatchFlow::start_at_station(state, preferred_station_id) {
                        Ok(flow) => {
                            self.dispatch_flow = Some(flow);
                            self.dispatch_returns_to_fleet = false;
                            self.notice = None;
                        }
                        Err(message) => self.notice = Some(message.into()),
                    }
                } else {
                    self.notice = Some(
                        "Unconnected Settlements are inspect-only; dispatch/construction unavailable."
                            .into(),
                    );
                }
            }
            _ => {}
        }
        ShellAction::Continue
    }

    /// Keeps a rejected confirmation visible to explain the actual current-state cause.
    pub fn reject_manual_dispatch(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(flow) = &mut self.dispatch_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a successful proposal after the application boundary persisted it.
    pub fn confirm_manual_dispatch(&mut self) {
        self.pending_action = None;
        self.dispatch_flow = None;
        if self.dispatch_returns_to_fleet {
            self.fleet_details_open = false;
            self.fleet_focus = FleetFocus::List;
        }
        self.dispatch_returns_to_fleet = false;
        self.notice = Some("Manual Dispatch authorised and saved.".into());
    }

    /// Publishes dispatch feedback only after the caller has saved and supplied
    /// the resulting game state.
    pub fn confirm_manual_dispatch_saved(&mut self, state: &GameState) {
        self.dispatch_flow = None;
        self.dispatch_returns_to_fleet = false;
        self.publish_pending_outcome(state);
    }

    /// Keeps a rejected purchase visible to explain the actual current-state cause.
    pub fn reject_purchase_train(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(flow) = &mut self.market_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a successful purchase proposal after the application boundary persisted it.
    pub fn confirm_purchase_train(&mut self) {
        self.pending_action = None;
        self.market_flow = None;
        self.notice = Some("Train purchase authorised and saved to the Fleet.".into());
    }

    /// Publishes purchase feedback only after the save boundary succeeded.
    pub fn confirm_purchase_train_saved(&mut self, state: &GameState) {
        self.market_flow = None;
        self.publish_pending_outcome(state);
    }

    /// Keeps a rejected resale visible to explain the actual current-state cause.
    pub fn reject_train_resale(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(flow) = &mut self.fleet_flow {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a saved resale and shows the actual proceeds credited to Company Funds.
    pub fn confirm_train_resale(&mut self, proceeds: crate::model::Money) {
        self.pending_action = None;
        self.fleet_flow = None;
        self.fleet_details_open = false;
        self.fleet_focus = FleetFocus::List;
        self.notice = Some(format!(
            "Train resold and saved. Sale proceeds of {} were added to Company Funds.",
            format_money(proceeds)
        ));
    }

    /// Publishes resale feedback only after the save boundary succeeded.
    pub fn confirm_train_resale_saved(&mut self, state: &GameState) {
        self.fleet_flow = None;
        self.fleet_details_open = false;
        self.fleet_focus = FleetFocus::List;
        self.publish_pending_outcome(state);
    }

    /// Publishes arrivals only from a successfully reconciled state transition.
    ///
    /// The shell retains no notification history: comparing the committed
    /// before/after states makes a repeated redraw or reconciliation a no-op.
    pub fn publish_committed_arrivals(&mut self, before: &GameState, after: &GameState) {
        let arrivals = after
            .financials
            .recent_journey_receipts
            .iter()
            .filter(|receipt| {
                !before
                    .financials
                    .recent_journey_receipts
                    .iter()
                    .any(|previous| previous.journey_id == receipt.journey_id)
            })
            .filter_map(|receipt| {
                before
                    .active_journeys
                    .iter()
                    .find(|journey| journey.id == receipt.journey_id)
                    .map(|journey| (journey, receipt.revenue))
            })
            .collect::<Vec<_>>();
        if arrivals.is_empty() {
            return;
        }

        let credited_revenue = arrivals.iter().fold(Money::ZERO, |total, (_, revenue)| {
            total.checked_add(*revenue).unwrap_or(total)
        });
        let funds = format_money(after.player_company.funds);
        let details = arrivals
            .iter()
            .map(|(journey, revenue)| {
                format!(
                    "Train {:02} arrived at {} — {} credited.",
                    journey.train_id.get(),
                    arrival_station_label(after, journey.destination_station_id),
                    signed_money(*revenue),
                )
            })
            .chain(std::iter::once(format!("Current Company Funds: {funds}.")))
            .collect();
        let summary = match arrivals.as_slice() {
            [(journey, revenue)] => format!(
                "Train {:02} arrived at {} — {} credited; Company Funds {}.",
                journey.train_id.get(),
                arrival_station_label(after, journey.destination_station_id),
                signed_money(*revenue),
                funds,
            ),
            _ => format!(
                "{} Journeys arrived — {} credited; Company Funds {}.",
                arrivals.len(),
                signed_money(credited_revenue),
                funds,
            ),
        };
        self.notice = None;
        self.action_outcome = Some(ActionOutcome {
            title: "Arrival summary",
            summary,
            details,
        });
    }

    fn publish_pending_outcome(&mut self, state: &GameState) {
        let Some(pending) = self.pending_action.take() else {
            return;
        };
        let funds_after = state.player_company.funds;
        let change = funds_after
            .checked_sub(pending.funds_before)
            .unwrap_or(Money::ZERO);
        let mut details = pending.details;
        details.push(format!(
            "Company Funds: {} → {} ({})",
            format_money(pending.funds_before),
            format_money(funds_after),
            signed_money(change),
        ));
        details
            .push("Saved successfully. This outcome is not stored as notification history.".into());
        self.notice = None;
        self.action_outcome = Some(ActionOutcome {
            title: "Saved action outcome",
            summary: format!(
                "{} saved — Company Funds {}.",
                pending.label,
                signed_money(change)
            ),
            details,
        });
    }

    /// Shows the persisted outcome of an explicitly confirmed Bankruptcy restart.
    pub fn confirm_restart_after_bankruptcy(&mut self) {
        self.active_view = View::Map;
        self.dispatch_flow = None;
        self.dispatch_returns_to_fleet = false;
        self.fleet_flow = None;
        self.market_flow = None;
        self.restart_confirmation = false;
        self.company_recovery_review_open = false;
        self.notice = Some(
            "Fresh game saved. The former Player Company save was preserved in a restart backup."
                .into(),
        );
    }

    /// Keeps a failed safe restart actionable without losing the prior save.
    pub fn reject_restart_after_bankruptcy(&mut self, error: impl Into<String>) {
        self.restart_confirmation = true;
        self.notice = Some(format!(
            "Safe restart failed; the existing save was not overwritten: {}",
            error.into()
        ));
    }

    /// Returns a readable instruction when the terminal cannot safely fit a view.
    pub fn resize_hint(&self, columns: u16, rows: u16) -> Option<&'static str> {
        if columns < MINIMUM_COLUMNS || rows < MINIMUM_ROWS {
            Some("Terminal too small — resize to at least 64 columns by 16 rows.")
        } else {
            None
        }
    }
}

/// Renders the current shell into a deterministic text copy of Ratatui's buffer.
///
/// This is intentionally narrow capture access for regression evidence. It
/// uses the same frame renderer as the live terminal while avoiding terminal
/// modes, clocks, and disk I/O.
pub fn capture_rendered_buffer(
    shell: &Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
) -> String {
    let mut rendered_shell = shell.clone();
    capture_rendered_buffer_mut(&mut rendered_shell, state, columns, rows)
}

/// Renders a deterministic buffer while retaining presentation state changes
/// such as a stateful Table viewport. This is limited to UI regression tests.
pub fn capture_rendered_buffer_mut(
    shell: &mut Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
) -> String {
    if columns == 0 || rows == 0 {
        return String::new();
    }

    let backend = TestBackend::new(columns, rows);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => match error {},
    };
    match terminal.draw(|frame| render_frame(frame, shell, state)) {
        Ok(_) => {}
        Err(error) => match error {},
    }

    let width = usize::from(columns);
    let height = usize::from(rows);
    let content = terminal.backend().buffer().content();
    let mut rendered = String::with_capacity((width + 1).saturating_mul(height));
    for row in content.chunks(width).take(height) {
        for cell in row {
            rendered.push_str(cell.symbol());
        }
        rendered.push('\n');
    }
    rendered
}

/// Returns the foreground and background colors painted at one cell by the
/// same deterministic renderer used for UI regression captures.
pub fn capture_rendered_cell_colors(
    shell: &Shell,
    state: &GameState,
    columns: u16,
    rows: u16,
    column: u16,
    row: u16,
) -> Option<(Color, Color)> {
    if columns == 0 || rows == 0 || column >= columns || row >= rows {
        return None;
    }
    let mut rendered_shell = shell.clone();
    let backend = TestBackend::new(columns, rows);
    let mut terminal = Terminal::new(backend).ok()?;
    terminal
        .draw(|frame| render_frame(frame, &mut rendered_shell, state))
        .ok()?;
    let index = usize::from(row)
        .checked_mul(usize::from(columns))?
        .checked_add(usize::from(column))?;
    let cell = terminal.backend().buffer().content().get(index)?;
    Some((cell.fg, cell.bg))
}

/// An error from the terminal shell or its supplied reconciliation boundary.
#[derive(Debug)]
pub enum RunError<E> {
    /// Terminal input, output, or mode configuration failed.
    Terminal(io::Error),
    /// Reconciliation failed before the shell could accept input.
    Reconcile(E),
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(error) => write!(formatter, "terminal error: {error}"),
            Self::Reconcile(error) => {
                write!(formatter, "could not reconcile elapsed time: {error}")
            }
        }
    }
}

impl<E: Error + 'static> Error for RunError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Terminal(error) => Some(error),
            Self::Reconcile(error) => Some(error),
        }
    }
}

/// Runs the four-view terminal shell until the player exits.
///
/// `command` is invoked while idle and immediately before every input event,
/// so a Player Company's due Journeys settle before the next action is accepted.
/// It also owns persisted Manual Dispatch confirmation. The terminal is restored
/// on ordinary errors and while unwinding a panic.
pub fn run_terminal<E>(
    initial_state: GameState,
    mut command: impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut terminal = TerminalSession::enter().map_err(RunError::Terminal)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_event_loop(&mut terminal, initial_state, &mut command)
    }));
    let restore_result = terminal.restore();

    match result {
        Ok(Ok(())) => restore_result.map_err(RunError::Terminal),
        Ok(Err(error)) => {
            restore_result.map_err(RunError::Terminal)?;
            Err(error)
        }
        Err(payload) => {
            let _ = restore_result;
            resume_unwind(payload);
        }
    }
}

fn run_event_loop<E>(
    terminal: &mut TerminalSession,
    mut state: GameState,
    command: &mut impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut shell = Shell::new();

    loop {
        terminal
            .draw(&mut shell, &state)
            .map_err(RunError::Terminal)?;
        let before_reconciliation = state.clone();
        let reconciled_state = command(TerminalCommand::Reconcile {
            now: current_utc_seconds(),
        })
        .map_err(RunError::Reconcile)?;
        shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
        state = reconciled_state;

        if !event::poll(ARRIVAL_POLL_INTERVAL).map_err(RunError::Terminal)? {
            continue;
        }

        match event::read().map_err(RunError::Terminal)? {
            Event::Key(key) => {
                let before_reconciliation = state.clone();
                let reconciled_state = command(TerminalCommand::Reconcile {
                    now: current_utc_seconds(),
                })
                .map_err(RunError::Reconcile)?;
                shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
                state = reconciled_state;
                match shell.handle_key(key, &state) {
                    ShellAction::Exit => return Ok(()),
                    ShellAction::ManualDispatch {
                        train_id,
                        destination_station_id,
                    } => match command(TerminalCommand::ManualDispatch {
                        train_id,
                        destination_station_id,
                        now: current_utc_seconds(),
                    }) {
                        Ok(next_state) => {
                            let before_command = state.clone();
                            state = next_state;
                            shell.confirm_manual_dispatch_saved(&state);
                            shell.publish_committed_arrivals(&before_command, &state);
                        }
                        Err(error) => shell.reject_manual_dispatch(error.to_string()),
                    },
                    ShellAction::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                    } => match command(TerminalCommand::PurchaseTrain {
                        catalogue_index,
                        delivery_station_id,
                        now: current_utc_seconds(),
                    }) {
                        Ok(next_state) => {
                            let before_command = state.clone();
                            state = next_state;
                            shell.confirm_purchase_train_saved(&state);
                            shell.publish_committed_arrivals(&before_command, &state);
                        }
                        Err(error) => shell.reject_purchase_train(error.to_string()),
                    },
                    ShellAction::SellTrain { train_id } => {
                        match command(TerminalCommand::SellTrain {
                            train_id,
                            now: current_utc_seconds(),
                        }) {
                            Ok(next_state) => {
                                let before_command = state.clone();
                                state = next_state;
                                shell.confirm_train_resale_saved(&state);
                                shell.publish_committed_arrivals(&before_command, &state);
                            }
                            Err(error) => shell.reject_train_resale(error.to_string()),
                        }
                    }
                    ShellAction::RestartAfterBankruptcy => {
                        match command(TerminalCommand::RestartAfterBankruptcy {
                            world_seed: restart_seed(),
                            now: current_utc_seconds(),
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_restart_after_bankruptcy();
                            }
                            Err(error) => shell.reject_restart_after_bankruptcy(error.to_string()),
                        }
                    }
                    ShellAction::Continue => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn current_utc_seconds() -> UtcSeconds {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    UtcSeconds::from_unix_seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn restart_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    nanos ^ u64::from(std::process::id())
}

fn is_bankrupt(state: &GameState) -> bool {
    matches!(
        evaluate_financial_recovery(state),
        Ok(evaluation) if evaluation.status == FinancialStatus::Bankruptcy
    )
}

/// Draws the complete dashboard with Ratatui widgets. Crossterm supplies the
/// cross-platform terminal backend and events; Ratatui owns layout and paint.
fn render_frame(frame: &mut ratatui::Frame, shell: &mut Shell, state: &GameState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::terminal()), area);
    if let Some(hint) = shell.resize_hint(area.width, area.height) {
        frame.render_widget(
            Paragraph::new(hint)
                .block(
                    Block::default()
                        .borders(theme::THIN_BORDERS)
                        .border_style(theme::border())
                        .title(APPLICATION_NAME)
                        .title_style(theme::title())
                        .style(theme::panel()),
                )
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let [
        header_area,
        navigation_area,
        content_area,
        feedback_area,
        hints_area,
    ] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(7),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let now = state.last_processed_at;
    frame.render_widget(
        Paragraph::new(company_status_text(state, now, header_area.width))
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::border())
                    .title(company_title(state, header_area.width))
                    .title_style(theme::title())
                    .style(theme::panel()),
            )
            .style(theme::panel()),
        header_area,
    );

    let views = [View::Map, View::Trains, View::Company, View::BuyTrains];
    let selected = views
        .iter()
        .position(|view| *view == shell.active_view)
        .unwrap_or(0);
    let compact_tabs = navigation_area.width <= 80;
    let titles = views
        .iter()
        .map(|view| Line::from(tab_label(*view, compact_tabs)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Tabs::new(titles)
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::border())
                    .title("Navigation")
                    .title_style(theme::title())
                    .style(theme::panel()),
            )
            .style(theme::panel())
            .select(selected)
            .highlight_style(theme::active_tab())
            .divider(" | "),
        navigation_area,
    );

    if !is_bankrupt(state) && shell.active_view == View::Map && shell.dispatch_flow.is_none() {
        shell.map_split_visible = content_area.width >= 96 && content_area.height >= 14;
        if shell.map_focus.is_journeys() {
            if let Some(train_id) = shell.map_journey_selection.selected_train_id(state) {
                shell.fleet_selection.select_train_id(state, train_id);
            }
        }
        map::render_dashboard(
            frame,
            content_area,
            state,
            now,
            map::MapSelections {
                stations: &mut shell.map_selection,
                settlements: &mut shell.map_settlement_selection,
                journeys: &mut shell.map_journey_selection,
            },
            shell.map_focus,
            shell.map_details_open,
        );
    } else if !is_bankrupt(state)
        && shell.active_view == View::Trains
        && shell.fleet_flow.is_none()
        && shell.dispatch_flow.is_none()
    {
        shell.fleet_split_visible = content_area.width >= 96 && content_area.height >= 14;
        fleet::render_dashboard(
            frame,
            content_area,
            state,
            now,
            &mut shell.fleet_selection,
            shell.fleet_details_open,
            shell.fleet_focus == FleetFocus::Details,
        );
    } else if shell.active_view == View::Trains && shell.dispatch_flow.is_some() {
        if let Some(flow) = &mut shell.dispatch_flow {
            flow.render_panel(frame, content_area, state);
        }
    } else if shell.active_view == View::Trains {
        if let Some(flow) = &shell.fleet_flow {
            flow.render_review(frame, content_area, state);
        } else {
            frame.render_widget(
                Paragraph::new(fleet::render_at(state, now))
                    .block(
                        Block::default()
                            .borders(theme::THIN_BORDERS)
                            .border_style(theme::border())
                            .title(shell.active_view.label())
                            .title_style(theme::title())
                            .style(theme::panel()),
                    )
                    .style(theme::panel())
                    .wrap(Wrap { trim: false }),
                content_area,
            );
        }
    } else if shell.active_view == View::Company
        && shell.company_recovery_review_open
        && !is_bankrupt(state)
    {
        company::render_recovery_review(
            frame,
            content_area,
            state,
            &mut shell.company_recovery_selection,
        );
    } else if shell.active_view == View::Company && !is_bankrupt(state) {
        company::render_dashboard(
            frame,
            content_area,
            state,
            &mut shell.company_receipt_selection,
            shell.company_receipt_details_open,
        );
    } else if shell.active_view == View::Map && shell.dispatch_flow.is_some() {
        if let Some(flow) = &mut shell.dispatch_flow {
            flow.render_panel(frame, content_area, state);
        }
    } else if shell.active_view == View::BuyTrains
        && shell.market_flow.is_none()
        && !is_bankrupt(state)
    {
        market::render_dashboard(frame, content_area, state, &mut shell.market_selection);
    } else if shell.active_view == View::BuyTrains && shell.market_flow.is_some() {
        if let Some(flow) = &mut shell.market_flow {
            flow.render_panel(frame, content_area, state);
        }
    } else {
        let content = if is_bankrupt(state) {
            bankruptcy_text(shell.restart_confirmation)
        } else {
            match shell.active_view {
                View::Map => map::render_at(state, now),
                View::Trains => fleet::render_at(state, now),
                View::BuyTrains => match &shell.market_flow {
                    Some(flow) => flow.render(state),
                    None => market::render(state),
                },
                View::Company => company::render(state),
            }
        };
        frame.render_widget(
            Paragraph::new(content)
                .block(
                    Block::default()
                        .borders(theme::THIN_BORDERS)
                        .border_style(theme::border())
                        .title(shell.active_view.label())
                        .title_style(theme::title())
                        .style(theme::panel()),
                )
                .style(theme::panel())
                .wrap(Wrap { trim: false }),
            content_area,
        );
    }

    frame.render_widget(
        Paragraph::new(
            shell
                .action_outcome
                .as_ref()
                .map(|outcome| format!("{}  [Enter] Read  [Esc] Dismiss", outcome.summary))
                .or_else(|| shell.notice.clone())
                .unwrap_or_default(),
        )
        .style(if shell.action_outcome.is_some() {
            theme::success()
        } else {
            theme::feedback()
        })
        .wrap(Wrap { trim: true }),
        feedback_area,
    );

    let controls = contextual_controls(shell, state, hints_area.width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("FOCUS ", theme::focused_title()),
            Span::styled(controls, theme::hint()),
        ]))
        .style(theme::hint())
        .wrap(Wrap { trim: true }),
        hints_area,
    );

    if shell.help_visible {
        render_help_overlay(frame, area, shell.help_offset);
    }
    if shell.outcome_details_open {
        if let Some(outcome) = &shell.action_outcome {
            render_outcome_overlay(frame, area, outcome);
        }
    }
}

fn contextual_controls(shell: &mut Shell, state: &GameState, width: u16) -> String {
    let compact = width <= 80;
    if shell.help_visible {
        return "[↑↓/J K] Scroll  [PgUp/Dn] Page  [? / H / Esc] Close  [Q] Quit".into();
    }
    if is_bankrupt(state) {
        return if shell.restart_confirmation {
            "[Enter] Confirm safe restart  [Esc] Cancel  [Q] Quit".into()
        } else {
            "[R] Safe restart  [?] Help  [Q] Quit".into()
        };
    }
    if let Some(flow) = &shell.dispatch_flow {
        return if flow.is_selecting_train() {
            "[↑↓] Train [Enter] Next [Esc] Cancel [?] Help [Q] Quit".into()
        } else if flow.is_selecting_destination() {
            if compact {
                "[↑↓] Route [Enter] Review [←] Back [Esc] Cancel [?] Help [Q] Quit".into()
            } else {
                "[↑↓/J K] Route  [PgUp/Dn] Scroll  [Enter] Review  [←/Back] Train  [Esc] Cancel  [?] Help  [Q] Quit".into()
            }
        } else if compact {
            "[Enter] Confirm [←] Back [Esc] Cancel [?] Help [Q] Quit".into()
        } else {
            "[Enter] Confirm dispatch  [←/Back] Route  [Esc] Cancel  [?] Help  [Q] Quit".into()
        };
    }
    if shell.active_view == View::Trains && shell.fleet_flow.is_some() {
        return "[Enter] Confirm resale  [Esc] Cancel  [?] Help  [Q] Quit".into();
    }
    if shell.active_view == View::Trains && shell.fleet_details_open {
        let action = shell
            .fleet_selection
            .selected_train_id(state)
            .and_then(|id| {
                state
                    .player_company
                    .fleet
                    .trains
                    .iter()
                    .find(|train| train.id == id)
            });
        let (dispatch, resale) = match action {
            Some(train) if matches!(train.status, TrainStatus::Ready { .. }) => {
                ("[D] Dispatch", "[S] Resale")
            }
            Some(_train) => ("[D] no: travel", "[S] no: travel"),
            None => ("[D] no: no Train", "[S] no: no Train"),
        };
        return format!("[Esc] Fleet  {dispatch}  {resale}  [?] Help  [Q] Quit");
    }
    if shell.active_view == View::Trains && shell.fleet_flow.is_none() {
        let action = shell
            .fleet_selection
            .selected_train_id(state)
            .and_then(|id| {
                state
                    .player_company
                    .fleet
                    .trains
                    .iter()
                    .find(|train| train.id == id)
            });
        let actions = match action {
            Some(train) if matches!(train.status, TrainStatus::Ready { .. }) => {
                "[D] Dispatch  [S] Resale"
            }
            Some(_) => "[D] no: travel  [S] no: travel",
            None => "[D] no: no Train  [S] no: no Train",
        };
        return if compact {
            format!("[↑↓] Fleet [Enter] View {actions} [?] Help [Q] Quit")
        } else {
            format!(
                "[↑↓/J K] Train  [PgUp/Dn] Scroll  [Enter] Inspect  {actions}  [Tab] Panel  [?] Help  [Q] Quit"
            )
        };
    }
    if shell.active_view == View::Map && shell.map_details_open {
        return "[Esc] Close detail  [Tab] Next panel  [?] Help  [Q] Quit".into();
    }
    if shell.active_view == View::Map {
        let focus = if shell.map_focus.is_stations() {
            "Stations"
        } else if shell.map_focus.is_journeys() {
            "Journeys"
        } else {
            "Settlements"
        };
        let action = if shell.map_focus.is_stations()
            && state
                .player_company
                .fleet
                .trains
                .iter()
                .any(|train| matches!(train.status, TrainStatus::Ready { .. }))
        {
            "[D] Dispatch"
        } else if shell.map_focus.is_stations() {
            "[D] no: READY Train"
        } else {
            "[D] no: inspect-only"
        };
        return if compact {
            format!("[↑↓] {focus} [Enter] Inspect {action} [?] Help [Q] Quit")
        } else {
            format!("[↑↓/J K] {focus} [Enter] Inspect {action} [Tab] Panel [?] Help [Q] Quit")
        };
    }
    if shell.active_view == View::Company {
        if shell.company_recovery_review_open {
            return "[↑↓/J K] Route  [Enter] Open  [Esc] Company  [?] Help  [Q] Quit".into();
        }
        if shell.company_receipt_details_open {
            return "[Esc] Receipts  [M/T/C/B] Navigate  [?] Help  [Q] Quit".into();
        }
        return "[↑↓] Receipt [Enter] Inspect [R] Recovery [?] Help [Q] Quit".into();
    }
    if shell.active_view == View::BuyTrains {
        if let Some(flow) = &shell.market_flow {
            if flow.is_selecting_delivery() {
                return "[↑↓] Station [Enter] Review [←] Back [Esc] Cancel [?] Help [Q] Quit"
                    .into();
            }
            return "[Enter] Confirm purchase  [←/Back] Delivery  [Esc] Cancel  [?] Help  [Q] Quit"
                .into();
        }
        return "[↑↓/J K] Model  [Enter] Delivery  [?] Help  [Q] Quit".into();
    }
    "[M/T/C/B] Views  [Enter] Inspect  [?] Help  [Q] Quit".into()
}

fn company_title(state: &GameState, width: u16) -> String {
    let max_name_cells = if width >= 100 { 40 } else { 18 };
    format!(
        "{APPLICATION_NAME} · {}",
        shorten(&state.player_company.name, max_name_cells)
    )
}

fn company_status_text(state: &GameState, now: UtcSeconds, width: u16) -> String {
    let ready = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .count();
    let travelling = state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Travelling { .. }))
        .count();
    let eta = nearest_eta(state, now)
        .map(|remaining| format!("NEXT ETA {remaining}"))
        .unwrap_or_else(|| "NEXT ETA —".into());

    if width >= 100 {
        format!(
            "Company Funds {}  |  READY {}  |  TRAVELLING {}  |  {eta}",
            format_money(state.player_company.funds),
            ready,
            travelling,
        )
    } else {
        format!(
            "Funds {}  |  R {}  |  T {}  |  {}",
            format_money(state.player_company.funds),
            ready,
            travelling,
            eta.replace("NEXT ETA ", "ETA "),
        )
    }
}

fn nearest_eta(state: &GameState, now: UtcSeconds) -> Option<String> {
    state
        .active_journeys
        .iter()
        .map(|journey| {
            journey
                .arrives_at
                .unix_seconds()
                .saturating_sub(now.unix_seconds())
                .max(0) as u64
        })
        .min()
        .map(format_remaining_time)
}

fn format_remaining_time(seconds: u64) -> String {
    format::duration(seconds)
}

fn tab_label(view: View, compact: bool) -> String {
    let label = match (view, compact) {
        (View::Company, true) => "Co.",
        (View::BuyTrains, true) => "Buy",
        _ => view.label(),
    };
    format!("[{}] {label}", view.shortcut())
}

fn shorten(value: &str, max_characters: usize) -> String {
    let mut characters = value.chars();
    let shortened = characters.by_ref().take(max_characters).collect::<String>();
    if characters.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

const HELP_PAGE_STEP: usize = 5;

const HELP_LINES: [&str; 40] = [
    "Global controls",
    "[M] Map  [T] Fleet  [C] Company  [B] Buy Trains switch primary views.",
    "[?] or [H] opens help. [Q] or Ctrl-C exits RailQ outside text entry.",
    "[Tab]/[Shift-Tab] changes visible panel focus; the footer names active controls.",
    "",
    "Panels and lists",
    "[Up]/[Down] or [J]/[K] changes the focused selection.",
    "[PageUp]/[PageDown] scrolls lists. [Enter] inspects the selected item.",
    "Fleet: [D] starts destination selection for a selected READY Train; [S] opens resale.",
    "Map: [D] begins Manual Dispatch from the selected Rail Station.",
    "Company: [R] opens calculated recovery routes during Insolvency.",
    "",
    "Flows",
    "[Enter] advances or confirms only the action named in the footer.",
    "[Left]/[Backspace] returns to the prior flow step; [Esc] cancels the current flow.",
    "Unavailable actions state their reason. Confirmation remains at the application boundary.",
    "Saved action outcomes remain in the feedback row: Enter reads details; Esc or A acknowledges.",
    "",
    "Current flow reminders",
    "Manual Dispatch: select a READY Train, select a destination Rail Station, then review.",
    "Train purchase: select a catalogue Train, choose its delivery Rail Station, then review.",
    "Train resale: review the selected READY Train before confirming its sale.",
    "No ordinary key reaches a covered panel while this help page is open.",
    "",
    "RailQ terminology",
    "The Player Company owns its Fleet and operates passenger Journeys.",
    "The Rail Authority owns the public Rail Network and its Rail Stations.",
    "A Journey is one physical movement of a Train; it is not a Passenger Service.",
    "",
    "Safety",
    "Opening, closing, scrolling, or resizing help does not authorise an action.",
    "It preserves the current focus, list position, and any pending proposal.",
    "Save and transaction checks still occur only after an explicit confirmation.",
    "",
    "Layout",
    "Wide terminals show help as a centred overlay above the control room.",
    "Compact terminals show it as a focused page so every visible row remains readable.",
    "Resize at any time; close help to resume the same workspace.",
    "",
    "[↑↓/J K] Scroll help  [PgUp/Dn] Page  [? / H / Esc] Return",
];

fn render_help_overlay(frame: &mut ratatui::Frame, area: Rect, offset: usize) {
    let compact = area.width < 96 || area.height < 26;
    let overlay_area = if compact {
        area
    } else {
        let width = area.width.saturating_mul(4) / 5;
        let height = area.height.saturating_mul(4) / 5;
        Rect::new(
            area.x
                .saturating_add((area.width.saturating_sub(width)) / 2),
            area.y
                .saturating_add((area.height.saturating_sub(height)) / 2),
            width,
            height,
        )
    };
    let visible_lines = usize::from(overlay_area.height.saturating_sub(2));
    let max_offset = HELP_LINES.len().saturating_sub(visible_lines.max(1));
    let offset = offset.min(max_offset);
    let content = HELP_LINES
        .iter()
        .skip(offset)
        .take(visible_lines)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let title = if compact {
        "Help · focused page"
    } else {
        "Keyboard help"
    };

    frame.render_widget(Clear, overlay_area);
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::focused_border())
                    .title(title)
                    .title_style(theme::focused_title())
                    .style(theme::panel()),
            )
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        overlay_area,
    );
}

fn render_outcome_overlay(frame: &mut ratatui::Frame, area: Rect, outcome: &ActionOutcome) {
    let compact = area.width < 96 || area.height < 26;
    let overlay_area = if compact {
        area
    } else {
        Rect::new(
            area.x.saturating_add(area.width / 10),
            area.y.saturating_add(area.height / 5),
            area.width.saturating_mul(4) / 5,
            area.height.saturating_mul(3) / 5,
        )
    };
    let mut lines = vec![
        Line::styled(&outcome.summary, theme::success()),
        Line::from(""),
    ];
    lines.extend(outcome.details.iter().cloned().map(Line::from));
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "Esc · return to notice   Enter/A · acknowledge",
        theme::hint(),
    ));
    frame.render_widget(Clear, overlay_area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::focused_border())
                    .title(outcome.title)
                    .title_style(theme::focused_title())
                    .style(theme::panel()),
            )
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        overlay_area,
    );
}

fn bankruptcy_text(restart_confirmation: bool) -> String {
    if restart_confirmation {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "",
            "Safe restart review: a fresh game is created only after this Player Company save is preserved in a unique archive backup.",
            "Press Enter to confirm the safe restart, Esc to keep the Bankrupt save, or Q to exit.",
        ]
        .join("\n")
    } else {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "Normal operations are disabled. You may exit safely or start a fresh game.",
            "",
            "Press R to review a safe restart. The existing Player Company save is archived first and is never silently overwritten.",
            "Press Q to exit or ? for keyboard help.",
        ]
        .join("\n")
    }
}

fn format_money(money: crate::model::Money) -> String {
    format::money(money)
}

fn signed_money(money: Money) -> String {
    let amount = format_money(money);
    if money.cents() > 0 {
        format!("+{amount}")
    } else {
        amount
    }
}

fn arrival_station_label(state: &GameState, station_id: RailStationId) -> String {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return format!("Rail Station {}", station_id.get());
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map(|settlement| format!("{} Rail Station", settlement.name))
        .unwrap_or_else(|| format!("Rail Station {}", station_id.get()))
}

fn pending_dispatch(
    state: &GameState,
    train_id: TrainId,
    destination_station_id: RailStationId,
) -> PendingAction {
    let model: String = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or_else(|| "unknown model".into(), |train| train.model_name.clone());
    PendingAction {
        label: format!("Manual Dispatch · Train {:02} ({model})", train_id.get()),
        details: vec![format!(
            "Journey authorised toward Rail Station {}. Departure costs and arrival revenue follow the saved Journey quote.",
            destination_station_id.get()
        )],
        funds_before: state.player_company.funds,
    }
}

fn pending_purchase(
    state: &GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
) -> PendingAction {
    let model: String = state
        .rules
        .balance
        .diesel_catalogue()
        .get(catalogue_index)
        .map_or_else(
            || "selected catalogue Train".into(),
            |train| train.name().into(),
        );
    PendingAction {
        label: format!("Train purchase · {model}"),
        details: vec![format!(
            "Delivered to Rail Station {} after the purchase was persisted.",
            delivery_station_id.get()
        )],
        funds_before: state.player_company.funds,
    }
}

fn pending_resale(state: &GameState, train_id: TrainId) -> PendingAction {
    let model: String = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or_else(|| "unknown model".into(), |train| train.model_name.clone());
    PendingAction {
        label: format!("Train resale · Train {:02} ({model})", train_id.get()),
        details: vec!["Sale proceeds were credited only after the save succeeded.".into()],
        funds_before: state.player_company.funds,
    }
}

/// RAII guard for the terminal modes owned by the shell.
struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    active: bool,
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }

        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, Show, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(error);
            }
        };

        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn draw(&mut self, shell: &mut Shell, state: &GameState) -> io::Result<()> {
        self.terminal
            .draw(|frame| render_frame(frame, shell, state))
            .map(|_| ())
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;

        let raw_mode_result = disable_raw_mode();
        let screen_result = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen);
        let cursor_result = self.terminal.show_cursor();
        raw_mode_result.and(screen_result).and(cursor_result)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            world::create_new_game,
        },
    };

    use super::{Shell, ShellAction, View, capture_rendered_buffer, theme};

    #[test]
    fn routes_the_four_primary_views() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        for (key, expected_view) in [
            ('t', View::Trains),
            ('c', View::Company),
            ('b', View::BuyTrains),
            ('m', View::Map),
        ] {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    &state
                ),
                ShellAction::Continue
            );
            assert_eq!(shell.active_view(), expected_view);
        }
    }

    #[test]
    fn routes_quit_and_control_c_to_clean_exit() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &state
            ),
            ShellAction::Exit
        );
    }

    #[test]
    fn small_terminal_gets_a_resize_hint() {
        assert!(Shell::new().resize_hint(63, 16).is_some());
        assert!(Shell::new().resize_hint(64, 15).is_some());
        assert!(Shell::new().resize_hint(64, 16).is_none());
    }

    #[test]
    fn narrow_terminal_frame_renders_resize_hint_without_panicking() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut shell = Shell::new();
        let state = create_new_game(42, "Narrow Passenger", UtcSeconds::from_unix_seconds(1_000));

        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Terminal too small"));
        assert!(rendered.contains("RailQ"));
    }

    #[test]
    fn control_room_shell_reports_company_status_with_adaptive_tabs_and_semantic_surfaces() {
        let started_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Northstar Passenger", started_at);
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        dispatch_journey(&mut state, train_id, service_id, started_at).unwrap();
        let mut shell = Shell::new();
        let funds = super::format_money(state.player_company.funds);

        let wide = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(wide.contains("RailQ · Northstar Passenger"));
        assert!(wide.contains(&format!("Company Funds {funds}")));
        assert!(wide.contains("READY 0"));
        assert!(wide.contains("TRAVELLING 1"));
        assert!(wide.contains("NEXT ETA"));
        assert!(wide.contains("[M] Map"));
        assert!(wide.contains("[T] Fleet"));
        assert!(wide.contains("[Q] Quit"));

        let compact = capture_rendered_buffer(&shell, &state, 80, 24);
        assert!(compact.contains(&format!("Funds {funds}")));
        assert!(compact.contains("R 0"));
        assert!(compact.contains("T 1"));
        assert!(compact.contains("ETA"));
        assert!(compact.contains("[C] Co."));
        assert!(compact.contains("[B] Buy"));
        assert!(compact.contains("[Q] Quit"));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let cells = terminal.backend().buffer().content();
        assert!(cells.iter().any(|cell| cell.bg == theme::BACKGROUND));
        assert!(cells.iter().any(|cell| cell.bg == theme::PANEL));
        assert!(cells.iter().any(|cell| cell.fg == theme::ACCENT));

        let feedback_state = create_new_game(42, "Feedback Passenger", started_at);
        let mut feedback_shell = Shell::new();
        feedback_shell.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &feedback_state,
        );
        let feedback = capture_rendered_buffer(&feedback_shell, &feedback_state, 120, 40);
        let feedback_lines = feedback.lines().collect::<Vec<_>>();
        assert!(feedback_lines[38].contains("No READY Train"));
        assert!(feedback_lines[39].contains("[Q] Quit"));

        let feedback_backend = TestBackend::new(120, 40);
        let mut feedback_terminal = Terminal::new(feedback_backend).unwrap();
        feedback_terminal
            .draw(|frame| super::render_frame(frame, &mut feedback_shell, &feedback_state))
            .unwrap();
        let feedback_cell = &feedback_terminal.backend().buffer().content()[120 * 38];
        assert_eq!(feedback_cell.symbol(), "N");
        assert_eq!(feedback_cell.fg, theme::WARNING);
        assert_eq!(feedback_cell.bg, theme::BACKGROUND);
    }

    #[test]
    fn fleet_table_marks_the_selected_train_with_the_accent_surface() {
        let started_at = UtcSeconds::from_unix_seconds(1_000);
        let mut state = create_new_game(42, "Fleet Selection", started_at);
        state.player_company.funds = Money::from_cents(1_000_000);
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        shell.handle_key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &state,
        );
        shell.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &state);

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render_frame(frame, &mut shell, &state))
            .unwrap();
        let marker = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == ">")
            .unwrap();
        assert_eq!(marker.fg, theme::BACKGROUND);
        assert_eq!(marker.bg, theme::ACCENT);
    }

    #[test]
    fn map_routes_a_keyboard_manual_dispatch_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('d')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Down), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::ManualDispatch {
                train_id,
                destination_station_id: RailStationId::new(3),
            }
        );
    }

    #[test]
    fn buy_trains_routes_only_an_explicit_confirmation_to_the_application_boundary() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('b')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Down), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::PurchaseTrain {
                catalogue_index: 1,
                delivery_station_id: RailStationId::new(1),
            }
        );
    }

    #[test]
    fn fleet_resale_routes_only_a_confirmed_ready_train_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('t')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Char('s')), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::SellTrain { train_id }
        );
    }

    #[test]
    fn help_explains_all_keyboard_reachable_core_actions() {
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert!(shell.help_visible());
        let help = super::HELP_LINES.join("\n");
        for instruction in [
            "[M] Map",
            "[T] Fleet",
            "[C] Company",
            "[B] Buy Trains",
            "[D]",
            "[D] starts destination selection",
            "[Q]",
        ] {
            assert!(help.contains(instruction));
        }
        shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(!shell.help_visible());
    }

    #[test]
    fn bankruptcy_blocks_normal_actions_but_allows_exit_and_confirmed_safe_restart() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::ZERO;
        let mut shell = Shell::new();

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(shell.active_view(), View::Map);
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Continue
        );
        assert_eq!(
            shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state),
            ShellAction::RestartAfterBankruptcy
        );
        assert!(super::bankruptcy_text(true).contains("archive backup"));
        assert!(super::bankruptcy_text(true).contains("Press Enter to confirm"));

        let mut exiting_shell = Shell::new();
        assert_eq!(
            exiting_shell.handle_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &state
            ),
            ShellAction::Exit
        );
    }
}
