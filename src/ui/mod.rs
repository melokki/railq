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
    time::Duration,
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
    catalog::{model_for_train, train_catalogue},
    model::{
        GameState, InfrastructureProjectId, Money, RailStationId, ServiceId, TrainId, TrainNickname,
        TrainStatus, UtcSeconds, VehicleKeeperMark,
    },
    sim::{
        finance::{FinancialStatus, evaluate_financial_recovery},
        time::SettledJourney,
    },
};

pub mod authority;
pub mod bulletin;
pub mod company;
pub mod dispatch;
pub mod fleet;
pub mod format;
pub mod map;
pub mod market;
pub mod modal;
pub mod services;
pub mod start;
pub mod theme;

/// How frequently the shell checks for elapsed arrivals while no key is pressed.
pub const ARRIVAL_POLL_INTERVAL: Duration = Duration::from_millis(250);

const MINIMUM_COLUMNS: u16 = 64;
const MINIMUM_ROWS: u16 = 16;

/// The six primary views in the RailQ terminal shell.
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
    /// The public Rail Authority infrastructure programme.
    Authority,
    /// Persistent significant regional railway developments.
    Bulletin,
}

impl View {
    const fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Trains => "Trains",
            Self::Company => "Company",
            Self::BuyTrains => "Market",
            Self::Authority => "Authority",
            Self::Bulletin => "Bulletin",
        }
    }

    const fn number(self) -> char {
        match self {
            Self::Map => '1',
            Self::Trains => '2',
            Self::BuyTrains => '3',
            Self::Company => '4',
            Self::Authority => '5',
            Self::Bulletin => '6',
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
        service_id: ServiceId,
    },
    /// Confirmed player input requiring an application-boundary Train purchase.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
    /// Confirmed player input requiring an application-boundary Train resale.
    SellTrain { train_id: TrainId },
    /// Confirmed player input creating one persistent directional Passenger Service.
    CreatePassengerService {
        stop_station_ids: Vec<RailStationId>,
    },
    /// Confirmed player input updating one unused directional Passenger Service.
    UpdatePassengerService {
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
    },
    /// Confirmed player input deleting one unused Passenger Service.
    DeletePassengerService { service_id: ServiceId },
    /// Confirmed player input updating the Player Company's Vehicle Keeper Mark.
    UpdateCompanyVkm {
        vehicle_keeper_mark: VehicleKeeperMark,
    },
    /// Confirmed player contribution to one public infrastructure project.
    ContributeInfrastructure {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
    /// Confirmed player input changing or clearing one Train nickname.
    UpdateTrainNickname {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
    /// Confirmed Bankruptcy restart requiring an archived-save application action.
    RestartAfterBankruptcy,
}

/// One command accepted by the terminal shell at the application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalCommand {
    /// Reconcile elapsed demand and due Journey arrivals before presentation or input.
    Reconcile,
    /// Revalidate and authorise a player-requested Manual Dispatch.
    ManualDispatch {
        train_id: TrainId,
        service_id: ServiceId,
    },
    /// Revalidate and purchase a selected catalogue Train.
    PurchaseTrain {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
    /// Revalidate and resell a selected READY Train.
    SellTrain { train_id: TrainId },
    /// Create one directional Passenger Service.
    CreatePassengerService {
        stop_station_ids: Vec<RailStationId>,
    },
    /// Update one unused directional Passenger Service.
    UpdatePassengerService {
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
    },
    /// Delete one unused Passenger Service.
    DeletePassengerService { service_id: ServiceId },
    /// Persist a new Player Company Vehicle Keeper Mark.
    UpdateCompanyVkm {
        vehicle_keeper_mark: VehicleKeeperMark,
    },
    /// Persist a Player Company contribution to one Authority project.
    ContributeInfrastructure {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
    /// Persist a Train nickname change without touching its official EVN.
    UpdateTrainNickname {
        train_id: TrainId,
        nickname: Option<TrainNickname>,
    },
    /// Archive the Bankrupt Player Company save and start a fresh game.
    RestartAfterBankruptcy,
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

/// Presentation-only state shared by the six primary views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    active_view: View,
    dispatch_flow: Option<dispatch::DispatchFlow>,
    dispatch_returns_to_fleet: bool,
    map_location_selection: map::MapLocationSelection,
    world_details_visible: bool,
    services_open: bool,
    service_workspace: services::ServiceWorkspace,
    fleet_flow: Option<fleet::FleetFlow>,
    fleet_selection: fleet::FleetSelection,
    fleet_details_open: bool,
    fleet_split_visible: bool,
    market_flow: Option<market::MarketFlow>,
    market_selection: market::CatalogueSelection,
    authority_project_selection: authority::ProjectSelection,
    authority_contribution_review: Option<authority::ContributionReview>,
    bulletin_workspace: bulletin::BulletinWorkspace,
    company_receipt_selection: company::ReceiptSelection,
    company_receipt_details_open: bool,
    company_recovery_selection: company::RecoverySelection,
    company_recovery_review_open: bool,
    company_vkm_editor: Option<company::VkmEditor>,
    train_nickname_editor: Option<fleet::TrainNicknameEditor>,
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
            map_location_selection: map::MapLocationSelection::default(),
            world_details_visible: false,
            services_open: false,
            service_workspace: services::ServiceWorkspace::default(),
            fleet_flow: None,
            fleet_selection: fleet::FleetSelection::default(),
            fleet_details_open: false,
            fleet_split_visible: false,
            market_flow: None,
            market_selection: market::CatalogueSelection::default(),
            authority_project_selection: authority::ProjectSelection::default(),
            authority_contribution_review: None,
            bulletin_workspace: bulletin::BulletinWorkspace::default(),
            company_receipt_selection: company::ReceiptSelection::default(),
            company_receipt_details_open: false,
            company_recovery_selection: company::RecoverySelection::default(),
            company_recovery_review_open: false,
            company_vkm_editor: None,
            train_nickname_editor: None,
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

        if let Some(editor) = &mut self.train_nickname_editor {
            return match editor.handle_key(key.code) {
                fleet::TrainNicknameEditorAction::Continue => ShellAction::Continue,
                fleet::TrainNicknameEditorAction::Cancel => {
                    self.train_nickname_editor = None;
                    self.notice = Some("Train rename cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                fleet::TrainNicknameEditorAction::Confirm { train_id, nickname } => {
                    ShellAction::UpdateTrainNickname { train_id, nickname }
                }
            };
        }

        if let Some(editor) = &mut self.company_vkm_editor {
            return match editor.handle_key(key.code) {
                company::VkmEditorAction::Continue => ShellAction::Continue,
                company::VkmEditorAction::Cancel => {
                    self.company_vkm_editor = None;
                    self.notice = Some("VKM edit cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                company::VkmEditorAction::Confirm(vehicle_keeper_mark) => {
                    ShellAction::UpdateCompanyVkm {
                        vehicle_keeper_mark,
                    }
                }
            };
        }

        if matches!(key.code, KeyCode::Char('q' | 'Q'))
            || (matches!(key.code, KeyCode::Char('c' | 'C'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return ShellAction::Exit;
        }

        if self.help_visible {
            let help_line_count = help_lines(self, state).len();
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => {
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
                        .min(help_line_count.saturating_sub(1));
                }
                KeyCode::PageUp => {
                    self.help_offset = self.help_offset.saturating_sub(HELP_PAGE_STEP);
                }
                KeyCode::PageDown => {
                    self.help_offset = self
                        .help_offset
                        .saturating_add(HELP_PAGE_STEP)
                        .min(help_line_count.saturating_sub(1));
                }
                _ => {}
            }
            return ShellAction::Continue;
        }

        if self.outcome_details_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('i' | 'I') => {
                    self.outcome_details_open = false;
                }
                _ => {}
            }
            return ShellAction::Continue;
        }

        // Saved-action feedback is deliberately non-blocking. The previous
        // interaction model trapped every key behind an acknowledgement step,
        // which made routine actions such as dispatching consecutive Trains feel
        // modal even after the action had already succeeded.
        //
        // `i` keeps the detailed receipt available on demand; any other key
        // dismisses the passive outcome and is then handled normally in the same
        // input event.
        if self.action_outcome.is_some() {
            if matches!(key.code, KeyCode::Char('i' | 'I')) {
                self.outcome_details_open = true;
                return ShellAction::Continue;
            }
            self.action_outcome = None;
        }

        if matches!(key.code, KeyCode::Char('?')) {
            self.help_visible = true;
            self.help_offset = 0;
            return ShellAction::Continue;
        }

        if self.world_details_visible {
            match key.code {
                KeyCode::Esc | KeyCode::Char('w' | 'W') => {
                    self.world_details_visible = false;
                    self.notice = None;
                    return ShellAction::Continue;
                }
                KeyCode::Char(
                    '1' | '2' | '3' | '4' | '5' | '6' | 'm' | 'M' | 't' | 'T' | 'b' | 'B' | 'c' | 'C' | 'a' | 'A' | 'u' | 'U',
                ) => {
                    self.world_details_visible = false;
                }
                _ => return ShellAction::Continue,
            }
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

        if let Some(review) = self.authority_contribution_review {
            return match key.code {
                KeyCode::Esc => {
                    self.authority_contribution_review = None;
                    self.notice = Some("Infrastructure contribution cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                KeyCode::Enter => ShellAction::ContributeInfrastructure {
                    project_id: review.project_id,
                    amount: review.amount,
                },
                _ => ShellAction::Continue,
            };
        }

        if let Some(flow) = &mut self.dispatch_flow {
            return match flow.handle_key(key, state) {
                dispatch::DispatchFlowAction::Continue => ShellAction::Continue,
                dispatch::DispatchFlowAction::Cancel => {
                    self.dispatch_flow = None;
                    if self.dispatch_returns_to_fleet {
                        self.fleet_details_open = false;
                    }
                    self.dispatch_returns_to_fleet = false;
                    self.notice = Some("Manual Dispatch cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                dispatch::DispatchFlowAction::Confirm {
                    train_id,
                    service_id,
                } => {
                    self.pending_action = Some(pending_dispatch(state, train_id, service_id));
                    ShellAction::ManualDispatch {
                        train_id,
                        service_id,
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

        if self.services_open {
            let navigation_key = matches!(
                key.code,
                KeyCode::Char(
                    '1' | '2' | '3' | '4' | '5' | '6' | 'm' | 'M' | 't' | 'T' | 'b' | 'B' | 'c' | 'C' | 'a' | 'A' | 'u' | 'U'
                )
            );
            if navigation_key {
                self.services_open = false;
            } else {
                return match self.service_workspace.handle_key(key.code, state) {
                    services::ServiceWorkspaceAction::Continue => ShellAction::Continue,
                    services::ServiceWorkspaceAction::Close => {
                        self.services_open = false;
                        self.notice = None;
                        ShellAction::Continue
                    }
                    services::ServiceWorkspaceAction::Create { stop_station_ids } => {
                        ShellAction::CreatePassengerService { stop_station_ids }
                    }
                    services::ServiceWorkspaceAction::Update {
                        service_id,
                        stop_station_ids,
                    } => ShellAction::UpdatePassengerService {
                        service_id,
                        stop_station_ids,
                    },
                    services::ServiceWorkspaceAction::Delete { service_id } => {
                        ShellAction::DeletePassengerService { service_id }
                    }
                };
            }
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
                KeyCode::Char('1' | 'm' | 'M') => {
                    self.company_recovery_review_open = false;
                    self.active_view = View::Map;
                }
                KeyCode::Char('2' | 't' | 'T') => {
                    self.company_recovery_review_open = false;
                    self.active_view = View::Trains;
                }
                KeyCode::Char('3' | 'b' | 'B') => {
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
            KeyCode::Char('1' | 'm' | 'M') => {
                self.active_view = View::Map;
                self.services_open = false;
            }
            KeyCode::Char('2' | 't' | 'T') => {
                self.active_view = View::Trains;
                self.services_open = false;
                self.fleet_details_open = false;
                self.fleet_split_visible = false;
            }
            KeyCode::Char('4' | 'c' | 'C') => {
                self.active_view = View::Company;
                self.services_open = false;
                self.company_receipt_details_open = false;
                self.company_recovery_review_open = false;
                self.company_vkm_editor = None;
            }
            KeyCode::Char('v' | 'V')
                if self.active_view == View::Company && !self.company_receipt_details_open =>
            {
                self.company_vkm_editor = Some(company::VkmEditor::start(state));
                self.company_receipt_details_open = false;
                self.company_recovery_review_open = false;
                self.notice = None;
            }
            KeyCode::Char('r' | 'R')
                if self.active_view == View::Company && !self.company_receipt_details_open =>
            {
                if self
                    .company_recovery_selection
                    .selected_destination(state)
                    .is_some()
                {
                    self.company_recovery_review_open = true;
                    self.company_receipt_details_open = false;
                    self.notice = None;
                }
            }
            KeyCode::Char('3' | 'b' | 'B') => {
                self.active_view = View::BuyTrains;
                self.services_open = false;
            }
            KeyCode::Char('5' | 'a' | 'A') => {
                self.active_view = View::Authority;
                self.services_open = false;
            }
            KeyCode::Char('6' | 'u' | 'U') => {
                self.active_view = View::Bulletin;
                self.services_open = false;
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
            KeyCode::Enter if self.active_view == View::Trains && !self.fleet_split_visible => {
                if self.fleet_selection.selected_train_id(state).is_some() {
                    self.fleet_details_open = true;
                    self.notice = None;
                }
            }
            KeyCode::Esc if self.active_view == View::Trains && self.fleet_details_open => {
                self.fleet_details_open = false;
            }
            KeyCode::Char('r' | 'R' | 'n' | 'N') if self.active_view == View::Trains => {
                match self.fleet_selection.selected_train_id(state) {
                    Some(train_id) => match fleet::TrainNicknameEditor::start(state, train_id) {
                        Ok(editor) => {
                            self.train_nickname_editor = Some(editor);
                            self.notice = None;
                        }
                        Err(message) => self.notice = Some(message),
                    },
                    None => {
                        self.notice = Some("Select a Train before renaming it.".into());
                    }
                }
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
                        match dispatch::DispatchFlow::start_with_selected_train(state, train_id) {
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
            KeyCode::Char('f' | 'F') if self.active_view == View::Authority => {
                match self.authority_project_selection.selected_project_id(state) {
                    Some(project_id) => match authority::ContributionReview::start(state, project_id) {
                        Ok(review) => {
                            self.authority_contribution_review = Some(review);
                            self.notice = None;
                        }
                        Err(message) => self.notice = Some(message.into()),
                    },
                    None => self.notice = Some("Select an infrastructure project first.".into()),
                }
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Authority =>
            {
                self.authority_project_selection.handle_key(key.code, state);
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K' | 'f' | 'F')
                if self.active_view == View::Bulletin =>
            {
                self.bulletin_workspace.handle_key(key.code, state);
            }
            KeyCode::Char('w' | 'W') if self.active_view == View::Map => {
                self.world_details_visible = true;
                self.notice = None;
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('h' | 'H' | 'j' | 'J' | 'k' | 'K' | 'l' | 'L')
                if self.active_view == View::Map =>
            {
                self.map_location_selection.handle_key(key.code, state);
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
            KeyCode::Char('s' | 'S') if self.active_view == View::Map => {
                self.services_open = true;
                self.notice = None;
            }
            KeyCode::Char('d' | 'D') if self.active_view == View::Map => {
                match dispatch::DispatchFlow::start(state) {
                    Ok(flow) => {
                        self.dispatch_flow = Some(flow);
                        self.dispatch_returns_to_fleet = false;
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message.into()),
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
        self.notice = Some(format!(
            "Train resold and saved. Sale proceeds of {} were added to Company Funds.",
            format_money(proceeds)
        ));
    }

    /// Publishes resale feedback only after the save boundary succeeded.
    pub fn confirm_train_resale_saved(&mut self, state: &GameState) {
        self.fleet_flow = None;
        self.fleet_details_open = false;
        self.publish_pending_outcome(state);
    }

    pub fn reject_infrastructure_contribution(&mut self, error: impl Into<String>) {
        self.notice = Some(error.into());
    }

    pub fn confirm_infrastructure_contribution_saved(&mut self, state: &GameState) {
        let amount = self
            .authority_contribution_review
            .map(|review| review.amount)
            .unwrap_or(Money::ZERO);
        self.authority_contribution_review = None;
        self.notice = Some(format!(
            "Infrastructure contribution of {} saved. Company Funds {}.",
            format_money(amount),
            format_money(state.player_company.funds)
        ));
    }

    /// Publishes arrivals only from a successfully reconciled state transition.
    ///
    /// The shell retains no notification history: comparing the committed
    /// before/after states makes a repeated redraw or reconciliation a no-op.
    pub fn publish_committed_arrivals(&mut self, before: &GameState, after: &GameState) {
        let arrivals = before
            .active_journeys
            .iter()
            .filter(|journey| {
                !after
                    .active_journeys
                    .iter()
                    .any(|next| next.id == journey.id)
            })
            .map(|journey| SettledJourney {
                journey_id: journey.id,
                train_id: journey.train_id,
                destination_station_id: journey.destination_station_id,
                credited_revenue: journey
                    .operating_revenue
                    .checked_sub(journey.credited_revenue)
                    .unwrap_or(Money::ZERO),
            })
            .collect::<Vec<_>>();
        self.publish_settled_arrivals(after, &arrivals);
    }

    /// Shows arrivals supplied by a reconciliation that has already saved.
    ///
    /// Arrivals are routine simulation events, so they use the non-blocking
    /// feedback row instead of requiring explicit acknowledgement.
    pub fn publish_settled_arrivals(&mut self, state: &GameState, arrivals: &[SettledJourney]) {
        if arrivals.is_empty() {
            return;
        }

        let credited_revenue = arrivals.iter().fold(Money::ZERO, |total, arrival| {
            total.checked_add(arrival.credited_revenue).unwrap_or(total)
        });
        let funds = format_money(state.player_company.funds);
        let summary = match arrivals {
            [arrival] => format!(
                "Train {:02} arrived at {} — {} credited; Company Funds {}.",
                arrival.train_id.get(),
                arrival_station_label(state, arrival.destination_station_id),
                signed_money(arrival.credited_revenue),
                funds,
            ),
            _ => format!(
                "{} Journeys arrived — {} credited; Company Funds {}.",
                arrivals.len(),
                signed_money(credited_revenue),
                funds,
            ),
        };

        self.notice = Some(summary);
        self.outcome_details_open = false;
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

    /// Closes a saved Passenger Service creation and keeps the Services workspace open.
    pub fn confirm_passenger_service_created(&mut self, state: &GameState) {
        self.service_workspace.confirm_created(state);
        self.services_open = true;
        self.notice = Some("Passenger Service created and saved.".into());
    }

    /// Closes a saved Passenger Service edit and keeps the Services workspace open.
    pub fn confirm_passenger_service_updated(&mut self, state: &GameState) {
        self.service_workspace.confirm_updated(state);
        self.services_open = true;
        self.notice = Some("Passenger Service updated and saved.".into());
    }

    /// Keeps a rejected Passenger Service action visible in the creation workspace.
    pub fn reject_passenger_service_action(&mut self, error: impl Into<String>) {
        let message = error.into();
        self.service_workspace.reject_action(message.clone());
        self.notice = Some(message);
    }

    /// Closes a saved Passenger Service deletion and keeps the Services workspace open.
    pub fn confirm_passenger_service_deleted(&mut self, state: &GameState) {
        self.service_workspace.confirm_deleted(state);
        self.services_open = true;
        self.notice = Some("Passenger Service deleted and saved.".into());
    }

    /// Closes the Company VKM editor after a persisted update.
    pub fn confirm_company_vkm_saved(&mut self, state: &GameState) {
        self.company_vkm_editor = None;
        self.notice = Some(format!(
            "Company VKM updated and saved as {}.",
            state.player_company.vehicle_keeper_mark
        ));
    }

    /// Closes a saved Train nickname edit and reports its current label.
    pub fn confirm_train_nickname_saved(&mut self, state: &GameState) {
        let train_id = self
            .train_nickname_editor
            .as_ref()
            .map(fleet::TrainNicknameEditor::train_id);
        self.train_nickname_editor = None;
        let summary = train_id
            .and_then(|train_id| {
                state
                    .player_company
                    .fleet
                    .trains
                    .iter()
                    .find(|train| train.id == train_id)
            })
            .map(|train| match &train.nickname {
                Some(nickname) => format!(
                    "Train {:02} is now named {}.",
                    train.id.get(),
                    nickname.as_str()
                ),
                None => format!("Train {:02} nickname cleared.", train.id.get()),
            })
            .unwrap_or_else(|| "Train nickname saved.".into());
        self.notice = Some(summary);
    }

    /// Keeps the nickname editor open after an application-boundary rejection.
    pub fn reject_train_nickname_update(&mut self, error: impl Into<String>) {
        self.notice = Some(error.into());
    }

    /// Keeps the Company VKM editor open after an application-boundary rejection.
    pub fn reject_company_vkm_update(&mut self, error: impl Into<String>) {
        self.notice = Some(error.into());
    }

    /// Shows the persisted outcome of an explicitly confirmed Bankruptcy restart.
    pub fn confirm_restart_after_bankruptcy(&mut self) {
        self.active_view = View::Map;
        self.dispatch_flow = None;
        self.dispatch_returns_to_fleet = false;
        self.fleet_flow = None;
        self.market_flow = None;
        self.services_open = false;
        self.service_workspace = services::ServiceWorkspace::default();
        self.restart_confirmation = false;
        self.company_recovery_review_open = false;
        self.company_vkm_editor = None;
        self.train_nickname_editor = None;
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
    command: impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    run_terminal_with_arrivals(initial_state, Vec::new(), command)
}

/// Runs the terminal shell with arrivals committed while loading a save.
pub fn run_terminal_with_arrivals<E>(
    initial_state: GameState,
    startup_arrivals: Vec<SettledJourney>,
    mut command: impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut terminal = TerminalSession::enter().map_err(RunError::Terminal)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_event_loop(&mut terminal, initial_state, startup_arrivals, &mut command)
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
    startup_arrivals: Vec<SettledJourney>,
    command: &mut impl FnMut(TerminalCommand) -> Result<GameState, E>,
) -> Result<(), RunError<E>>
where
    E: fmt::Display,
{
    let mut shell = Shell::new();
    shell.publish_settled_arrivals(&state, &startup_arrivals);

    loop {
        terminal
            .draw(|frame| render_frame(frame, &mut shell, &state))
            .map_err(RunError::Terminal)?;
        let before_reconciliation = state.clone();
        let reconciled_state = command(TerminalCommand::Reconcile)
        .map_err(RunError::Reconcile)?;
        shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
        state = reconciled_state;

        if !event::poll(ARRIVAL_POLL_INTERVAL).map_err(RunError::Terminal)? {
            continue;
        }

        match event::read().map_err(RunError::Terminal)? {
            Event::Key(key) => {
                let before_reconciliation = state.clone();
                let reconciled_state = command(TerminalCommand::Reconcile)
                .map_err(RunError::Reconcile)?;
                shell.publish_committed_arrivals(&before_reconciliation, &reconciled_state);
                state = reconciled_state;
                match shell.handle_key(key, &state) {
                    ShellAction::Exit => return Ok(()),
                    ShellAction::ManualDispatch {
                        train_id,
                        service_id,
                    } => match command(TerminalCommand::ManualDispatch {
                        train_id,
                        service_id,
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
                    ShellAction::CreatePassengerService { stop_station_ids } => {
                        match command(TerminalCommand::CreatePassengerService {
                            stop_station_ids,
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_passenger_service_created(&state);
                            }
                            Err(error) => shell.reject_passenger_service_action(error.to_string()),
                        }
                    }
                    ShellAction::UpdatePassengerService {
                        service_id,
                        stop_station_ids,
                    } => match command(TerminalCommand::UpdatePassengerService {
                        service_id,
                        stop_station_ids,
                    }) {
                        Ok(next_state) => {
                            state = next_state;
                            shell.confirm_passenger_service_updated(&state);
                        }
                        Err(error) => shell.reject_passenger_service_action(error.to_string()),
                    },
                    ShellAction::DeletePassengerService { service_id } => {
                        match command(TerminalCommand::DeletePassengerService {
                            service_id,
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_passenger_service_deleted(&state);
                            }
                            Err(error) => shell.reject_passenger_service_action(error.to_string()),
                        }
                    }
                    ShellAction::UpdateCompanyVkm {
                        vehicle_keeper_mark,
                    } => {
                        match command(TerminalCommand::UpdateCompanyVkm {
                            vehicle_keeper_mark,
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_company_vkm_saved(&state);
                            }
                            Err(error) => shell.reject_company_vkm_update(error.to_string()),
                        }
                    }
                    ShellAction::ContributeInfrastructure { project_id, amount } => {
                        match command(TerminalCommand::ContributeInfrastructure {
                            project_id,
                            amount,
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_infrastructure_contribution_saved(&state);
                            }
                            Err(error) => {
                                shell.reject_infrastructure_contribution(error.to_string())
                            }
                        }
                    }
                    ShellAction::UpdateTrainNickname { train_id, nickname } => {
                        match command(TerminalCommand::UpdateTrainNickname {
                            train_id,
                            nickname,
                        }) {
                            Ok(next_state) => {
                                state = next_state;
                                shell.confirm_train_nickname_saved(&state);
                            }
                            Err(error) => shell.reject_train_nickname_update(error.to_string()),
                        }
                    }
                    ShellAction::RestartAfterBankruptcy => {
                        match command(TerminalCommand::RestartAfterBankruptcy) {
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

    let [header_area, navigation_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(7),
        Constraint::Length(4),
    ])
    .areas(area);

    let now = state.last_processed_at;
    frame.render_widget(
        Paragraph::new(shell_status_line(state, now, header_area.width)).style(theme::terminal()),
        header_area,
    );

    let views = [
        View::Map,
        View::Trains,
        View::BuyTrains,
        View::Company,
        View::Authority,
        View::Bulletin,
    ];
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
            .style(theme::navigation())
            .select(selected)
            .highlight_style(theme::active_tab())
            .divider("   "),
        navigation_area,
    );

    if !is_bankrupt(state) && shell.active_view == View::Map && shell.services_open {
        shell
            .service_workspace
            .render_base(frame, content_area, state);
    } else if !is_bankrupt(state) && shell.active_view == View::Map {
        map::render_operational_map(
            frame,
            content_area,
            state,
            &mut shell.map_location_selection,
        );
    } else if !is_bankrupt(state) && shell.active_view == View::Trains {
        shell.fleet_split_visible = content_area.width >= 96 && content_area.height >= 18;
        if shell.fleet_split_visible {
            shell.fleet_details_open = false;
        }
        fleet::render_dashboard(
            frame,
            content_area,
            state,
            now,
            &mut shell.fleet_selection,
            shell.fleet_details_open,
        );
    } else if shell.active_view == View::Trains {
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
    } else if shell.active_view == View::Company && !is_bankrupt(state) {
        company::render_dashboard(
            frame,
            content_area,
            state,
            &mut shell.company_receipt_selection,
            false,
        );
    } else if shell.active_view == View::BuyTrains && !is_bankrupt(state) {
        let delivery_flow_open = shell
            .market_flow
            .as_ref()
            .is_some_and(market::MarketFlow::is_selecting_delivery);
        if delivery_flow_open {
            if let Some(flow) = &mut shell.market_flow {
                flow.render_panel(frame, content_area, state);
            }
        } else {
            market::render_dashboard(frame, content_area, state, &mut shell.market_selection);
        }
    } else if shell.active_view == View::Authority && !is_bankrupt(state) {
        authority::render_dashboard(
            frame,
            content_area,
            state,
            now,
            &mut shell.authority_project_selection,
        );
    } else if shell.active_view == View::Bulletin && !is_bankrupt(state) {
        shell.bulletin_workspace.render(frame, content_area, state, now);
    } else {
        let content = if is_bankrupt(state) {
            bankruptcy_text(false)
        } else {
            match shell.active_view {
                View::Map => map::render_at(state, now),
                View::Trains => fleet::render_at(state, now),
                View::BuyTrains => match &shell.market_flow {
                    Some(flow) => flow.render(state),
                    None => market::render(state),
                },
                View::Company => company::render(state),
                View::Authority => authority::render(state, now),
                View::Bulletin => "Railway Bulletin".into(),
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

    render_footer(frame, footer_area, shell, state);

    // Focused workflows are a separate presentation layer.  The entire app is
    // first muted, then the modal is painted with the normal palette so input
    // ownership is obvious without making the dialog larger or brighter.
    if focused_modal_visible(shell, state) {
        modal::dim_backdrop(frame, area);
        render_focused_modal(frame, content_area, shell, state);
    }

    // Informational overlays can stack above an active workflow (for example
    // Help opened from Dispatch).  Each layer dims what is already underneath
    // it, which keeps the topmost interaction visually unambiguous.
    if shell.world_details_visible {
        modal::dim_backdrop(frame, area);
        render_world_details_overlay(frame, area, state);
    }
    if shell.help_visible {
        modal::dim_backdrop(frame, area);
        render_help_overlay(frame, area, shell, state);
    }
    if shell.outcome_details_open {
        if let Some(outcome) = &shell.action_outcome {
            modal::dim_backdrop(frame, area);
            render_outcome_overlay(frame, area, outcome);
        }
    }
}

fn focused_modal_visible(shell: &Shell, state: &GameState) -> bool {
    shell.authority_contribution_review.is_some()
        || shell.train_nickname_editor.is_some()
        || shell.company_vkm_editor.is_some()
        || shell.company_recovery_review_open
        || shell.company_receipt_details_open
        || (is_bankrupt(state) && shell.restart_confirmation)
        || shell.dispatch_flow.is_some()
        || shell.fleet_flow.is_some()
        || shell
            .market_flow
            .as_ref()
            .is_some_and(market::MarketFlow::is_confirming)
        || (shell.services_open && shell.service_workspace.has_modal())
}

fn render_focused_modal(
    frame: &mut ratatui::Frame,
    content_area: Rect,
    shell: &mut Shell,
    state: &GameState,
) {
    if let Some(review) = shell.authority_contribution_review {
        authority::render_contribution_review(frame, content_area, state, review);
        return;
    }
    if let Some(editor) = &shell.train_nickname_editor {
        fleet::render_nickname_editor(frame, content_area, editor, state);
        return;
    }
    if let Some(editor) = &shell.company_vkm_editor {
        company::render_vkm_editor(frame, content_area, editor, state);
        return;
    }
    if shell.company_recovery_review_open {
        company::render_recovery_review(
            frame,
            content_area,
            state,
            &mut shell.company_recovery_selection,
        );
        return;
    }
    if shell.company_receipt_details_open {
        company::render_receipt_modal(
            frame,
            content_area,
            state,
            &mut shell.company_receipt_selection,
        );
        return;
    }
    if is_bankrupt(state) && shell.restart_confirmation {
        render_bankruptcy_restart_confirmation(frame, content_area);
        return;
    }
    if let Some(flow) = &mut shell.dispatch_flow {
        flow.render_panel(frame, modal::workflow_rect(content_area), state);
        return;
    }
    if let Some(flow) = &shell.fleet_flow {
        flow.render_review(frame, content_area, state);
        return;
    }
    if shell
        .market_flow
        .as_ref()
        .is_some_and(market::MarketFlow::is_confirming)
    {
        if let Some(flow) = &mut shell.market_flow {
            flow.render_panel(frame, content_area, state);
        }
        return;
    }
    if shell.services_open && shell.service_workspace.has_modal() {
        shell
            .service_workspace
            .render_modal(frame, content_area, state);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FooterShortcut {
    key: String,
    action: String,
    enabled: bool,
}

impl FooterShortcut {
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

fn render_footer(frame: &mut ratatui::Frame, area: Rect, shell: &mut Shell, state: &GameState) {
    let mut lines = Vec::new();
    if let Some(outcome) = &shell.action_outcome {
        lines.push(Line::styled(
            format!("✓ {}", outcome.summary),
            theme::success(),
        ));
    } else if let Some(notice) = &shell.notice {
        lines.push(Line::styled(notice.clone(), theme::warning()));
    }

    let inner_width = area.width.saturating_sub(2);
    let outcome_shortcut = (shell.action_outcome.is_some() && !shell.outcome_details_open)
        .then(|| FooterShortcut::enabled("i", "Details"));
    let reserved = outcome_shortcut
        .as_ref()
        .map(|shortcut| shortcut_width(shortcut).saturating_add(1) as u16)
        .unwrap_or(0);
    let mut shortcuts = contextual_controls(shell, state, inner_width.saturating_sub(reserved));
    if let Some(shortcut) = outcome_shortcut {
        shortcuts.insert(0, shortcut);
    }
    lines.push(shortcut_line(&shortcuts));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::footer_border())
                    .style(theme::panel()),
            )
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn shortcut_width(shortcut: &FooterShortcut) -> usize {
    shortcut.key.chars().count() + shortcut.action.chars().count() + 3
}

fn shortcut_line_width(shortcuts: &[FooterShortcut]) -> usize {
    shortcuts
        .iter()
        .map(shortcut_width)
        .sum::<usize>()
        .saturating_add(shortcuts.len().saturating_sub(1))
}

fn push_shortcut_if_fits(
    shortcuts: &mut Vec<FooterShortcut>,
    shortcut: FooterShortcut,
    width: u16,
) {
    let added_separator = if shortcuts.is_empty() { 0 } else { 1 };
    let next_width = shortcut_line_width(shortcuts)
        .saturating_add(added_separator)
        .saturating_add(shortcut_width(&shortcut));
    if next_width <= usize::from(width) {
        shortcuts.push(shortcut);
    }
}

fn shortcut_line(shortcuts: &[FooterShortcut]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, shortcut) in shortcuts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", theme::shortcut_action()));
        }
        let key_style = if shortcut.enabled {
            theme::shortcut_key()
        } else {
            theme::shortcut_disabled()
        };
        let action_style = if shortcut.enabled {
            theme::shortcut_action()
        } else {
            theme::shortcut_disabled()
        };
        spans.push(Span::styled(format!("[{}]", shortcut.key), key_style));
        spans.push(Span::styled(format!(" {}", shortcut.action), action_style));
    }
    Line::from(spans)
}

fn contextual_controls(shell: &mut Shell, state: &GameState, width: u16) -> Vec<FooterShortcut> {
    let compact = width <= 78;
    let wide = width >= 104;

    if shell.help_visible {
        let mut actions = vec![
            FooterShortcut::enabled(if compact { "↑↓" } else { "↑↓/JK" }, "Scroll"),
            FooterShortcut::enabled("PgUp/PgDn", "Page"),
            FooterShortcut::enabled("?/Esc", "Close"),
        ];
        actions.push(FooterShortcut::enabled("Q", "Quit"));
        return actions;
    }
    if shell.world_details_visible {
        return vec![
            FooterShortcut::enabled("W/Esc", "Close"),
            FooterShortcut::enabled("?", "Help"),
            FooterShortcut::enabled("Q", "Quit"),
        ];
    }
    if shell.authority_contribution_review.is_some() {
        return vec![
            FooterShortcut::enabled("Enter", "Contribute"),
            FooterShortcut::enabled("Esc", "Cancel"),
            FooterShortcut::enabled("Q", "Quit"),
        ];
    }
    if shell.train_nickname_editor.is_some() {
        return vec![
            FooterShortcut::enabled("Enter", "Save"),
            FooterShortcut::enabled("Backspace", "Delete"),
            FooterShortcut::enabled("Esc", "Cancel"),
        ];
    }
    if shell.company_vkm_editor.is_some() {
        return vec![
            FooterShortcut::enabled("Enter", "Save"),
            FooterShortcut::enabled("Backspace", "Delete"),
            FooterShortcut::enabled("Esc", "Cancel"),
        ];
    }
    if shell.outcome_details_open {
        return vec![
            FooterShortcut::enabled("i", "Close"),
            FooterShortcut::enabled("Esc", "Close"),
        ];
    }
    if is_bankrupt(state) {
        return if shell.restart_confirmation {
            vec![
                FooterShortcut::enabled("Enter", "Restart"),
                FooterShortcut::enabled("Esc", "Cancel"),
                FooterShortcut::enabled("Q", "Quit"),
            ]
        } else {
            vec![
                FooterShortcut::enabled("R", "Safe restart"),
                FooterShortcut::enabled("?", "Help"),
                FooterShortcut::enabled("Q", "Quit"),
            ]
        };
    }

    let mut actions = if let Some(flow) = &shell.dispatch_flow {
        if flow.is_selecting_train() {
            vec![
                FooterShortcut::enabled(if compact { "↑↓" } else { "↑↓/JK" }, "Train"),
                FooterShortcut::enabled("Enter", "Next"),
                FooterShortcut::enabled("Esc", "Cancel"),
            ]
        } else if flow.is_selecting_service() {
            let mut items = vec![FooterShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Service",
            )];
            if wide {
                items.push(FooterShortcut::enabled("PgUp/PgDn", "Scroll"));
            }
            items.push(FooterShortcut::enabled("Enter", "Review"));
            items.push(FooterShortcut::enabled("←", "Back"));
            items.push(FooterShortcut::enabled("Esc", "Cancel"));
            items
        } else {
            vec![
                FooterShortcut::enabled("Enter", "Confirm"),
                FooterShortcut::enabled("←", "Back"),
                FooterShortcut::enabled("Esc", "Cancel"),
            ]
        }
    } else if shell.active_view == View::Trains && shell.fleet_flow.is_some() {
        vec![
            FooterShortcut::enabled("Enter", "Resell"),
            FooterShortcut::enabled("Esc", "Cancel"),
        ]
    } else if shell.active_view == View::Trains && shell.fleet_details_open {
        let mut items = vec![FooterShortcut::enabled("Esc", "Back")];
        items.extend(fleet_action_shortcuts(shell, state));
        items
    } else if shell.active_view == View::Trains && shell.fleet_flow.is_none() {
        if state.player_company.fleet.trains.is_empty() {
            let mut items = vec![FooterShortcut::enabled("3", "Market")];
            items.extend(fleet_action_shortcuts(shell, state));
            items
        } else {
            let mut items = vec![FooterShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Train",
            )];
            if wide {
                items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
            }
            if !shell.fleet_split_visible {
                items.push(FooterShortcut::enabled("Enter", "Details"));
            }
            items.extend(fleet_action_shortcuts(shell, state));
            items
        }
    } else if shell.active_view == View::Map && shell.services_open {
        shell
            .service_workspace
            .footer_shortcuts(compact, wide, state)
            .into_iter()
            .map(|(key, action, enabled)| {
                if enabled {
                    FooterShortcut::enabled(key, action)
                } else {
                    FooterShortcut::disabled(key, action)
                }
            })
            .collect()
    } else if shell.active_view == View::Map {
        let train_count = state.player_company.fleet.trains.len();
        let ready = state
            .player_company
            .fleet
            .trains
            .iter()
            .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
            .count();
        let mut items = vec![FooterShortcut::enabled(
            if compact {
                "↑↓←→"
            } else {
                "↑↓←→/HJKL"
            },
            "Station",
        )];
        if train_count == 0 {
            items.push(FooterShortcut::enabled("3", "Market"));
        } else if ready == 0 {
            // The header already owns Fleet status. Keep the footer purely
            // action-oriented and let disabled styling communicate availability.
            items.push(FooterShortcut::disabled("D", "Dispatch"));
        } else {
            items.push(FooterShortcut::enabled("D", "Dispatch"));
        }
        items.push(FooterShortcut::enabled("S", "Services"));
        items.push(FooterShortcut::enabled("W", "World"));
        items
    } else if shell.active_view == View::Company {
        let recovery_available =
            evaluate_financial_recovery(state)
                .ok()
                .is_some_and(|evaluation| {
                    evaluation.status == FinancialStatus::Insolvent
                        && !evaluation.recovery_options.is_empty()
                });
        if shell.company_recovery_review_open {
            let mut items = vec![FooterShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Route",
            )];
            if wide {
                items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
            }
            items.push(FooterShortcut::enabled("Enter", "Review"));
            items.push(FooterShortcut::enabled("Esc", "Back"));
            items
        } else if shell.company_receipt_details_open {
            vec![FooterShortcut::enabled("Esc", "Back")]
        } else {
            let mut items = Vec::new();
            if !state.financials.recent_journey_receipts.is_empty() {
                items.push(FooterShortcut::enabled(
                    if compact { "↑↓" } else { "↑↓/JK" },
                    "Receipt",
                ));
                if wide {
                    items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
                }
                items.push(FooterShortcut::enabled("Enter", "Inspect"));
            }
            items.push(FooterShortcut::enabled("V", "Edit VKM"));
            items.push(if recovery_available {
                FooterShortcut::enabled("R", "Recovery")
            } else {
                FooterShortcut::disabled("R", "Recovery")
            });
            items
        }
    } else if shell.active_view == View::Authority {
        if state
            .region
            .rail_authority
            .infrastructure_projects
            .is_empty()
        {
            vec![FooterShortcut::disabled("↑↓", "Project")]
        } else {
            let mut items = vec![FooterShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Project",
            )];
            if wide {
                items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
            }
            let can_contribute = shell
                .authority_project_selection
                .selected_project_id(state)
                .and_then(|project_id| authority::ContributionReview::start(state, project_id).ok())
                .is_some();
            items.push(if can_contribute {
                FooterShortcut::enabled("F", "Contribute")
            } else {
                FooterShortcut::disabled("F", "Contribute")
            });
            items
        }
    } else if shell.active_view == View::Bulletin {
        let mut items = vec![FooterShortcut::enabled(
            if compact { "↑↓" } else { "↑↓/JK" },
            "Item",
        )];
        if wide {
            items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
        }
        items.push(FooterShortcut::enabled(
            "F",
            format!("Filter · {}", shell.bulletin_workspace.filter_label()),
        ));
        items
    } else if shell.active_view == View::BuyTrains {
        if let Some(flow) = &shell.market_flow {
            if flow.is_selecting_delivery() {
                let mut items = vec![FooterShortcut::enabled(
                    if compact { "↑↓" } else { "↑↓/JK" },
                    "Station",
                )];
                if wide {
                    items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
                }
                items.push(FooterShortcut::enabled("Enter", "Review"));
                items.push(FooterShortcut::enabled("←", "Model"));
                items.push(FooterShortcut::enabled("Esc", "Cancel"));
                items
            } else {
                vec![
                    FooterShortcut::enabled("Enter", "Purchase"),
                    FooterShortcut::enabled("←", "Back"),
                    FooterShortcut::enabled("Esc", "Cancel"),
                ]
            }
        } else {
            let mut items = vec![FooterShortcut::enabled(
                if compact { "↑↓" } else { "↑↓/JK" },
                "Model",
            )];
            if wide {
                items.push(FooterShortcut::enabled("PgUp/PgDn", "Page"));
            }
            let can_buy = shell
                .market_selection
                .selected_catalogue_index(state)
                .is_some_and(|catalogue_index| {
                    market::purchase_action_available(state, catalogue_index)
                });
            items.push(if can_buy {
                FooterShortcut::enabled("Enter", "Buy")
            } else {
                FooterShortcut::disabled("Enter", "Buy")
            });
            items
        }
    } else {
        vec![FooterShortcut::enabled("Enter", "Details")]
    };

    // Keep utility actions predictable without forcing the task-specific
    // controls to wrap. At narrow widths they disappear only when they do not
    // fit; every shortcut continues to work even when omitted from the hint.
    push_shortcut_if_fits(&mut actions, FooterShortcut::enabled("?", "Help"), width);
    push_shortcut_if_fits(&mut actions, FooterShortcut::enabled("Q", "Quit"), width);
    actions
}

fn fleet_action_shortcuts(shell: &mut Shell, state: &GameState) -> Vec<FooterShortcut> {
    let selected = shell
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
    let has_selection = selected.is_some();
    let is_ready = selected.is_some_and(|train| matches!(&train.status, TrainStatus::Ready { .. }));

    vec![
        if has_selection {
            FooterShortcut::enabled("R", "Rename")
        } else {
            FooterShortcut::disabled("R", "Rename")
        },
        if is_ready {
            FooterShortcut::enabled("D", "Dispatch")
        } else {
            FooterShortcut::disabled("D", "Dispatch")
        },
        if is_ready {
            FooterShortcut::enabled("S", "Sell")
        } else {
            FooterShortcut::disabled("S", "Sell")
        },
    ]
}

fn shell_status_line(state: &GameState, now: UtcSeconds, width: u16) -> String {
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
        .map(|remaining| format!("ETA {remaining}"))
        .unwrap_or_else(|| "ETA —".into());

    if width >= 100 {
        format!(
            "{APPLICATION_NAME} · {}   Company Funds {}   READY {}   TRAVELLING {}   {eta}",
            shorten(&state.player_company.name, 34),
            format_money(state.player_company.funds),
            ready,
            travelling,
        )
    } else {
        format!(
            "{APPLICATION_NAME} · {}   Funds {}   R {}   T {}   {eta}",
            shorten(&state.player_company.name, 16),
            format_money(state.player_company.funds),
            ready,
            travelling,
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
        (View::Trains, true) => "Trn",
        (View::BuyTrains, true) => "Mkt",
        (View::Company, true) => "Co",
        (View::Authority, true) => "Auth",
        (View::Bulletin, true) => "News",
        (View::Trains, false) => "Trains",
        (View::BuyTrains, false) => "Market",
        _ => view.label(),
    };
    format!("{} {label}", view.number())
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

fn help_lines(shell: &Shell, state: &GameState) -> Vec<String> {
    let mut lines = vec![
        "Navigation".into(),
        "1 Map   2 Trains   3 Market   4 Company   5 Authority   6 Bulletin".into(),
        "m / t / b / c / a / u also switch workspaces".into(),
        String::new(),
    ];

    if is_bankrupt(state) {
        lines.extend([
            "Current · Bankruptcy".into(),
            "r Review a safe restart".into(),
            "Enter Confirm restart when the review is open".into(),
            "Esc Cancel restart review".into(),
        ]);
        return lines;
    }

    if shell.world_details_visible {
        lines.extend([
            "Current · World Details".into(),
            "w / Esc Return to Map".into(),
            "The railway registration belongs to the Region and remains stable for this save."
                .into(),
        ]);
        return lines;
    }

    if let Some(flow) = &shell.dispatch_flow {
        lines.push("Current · Manual Dispatch".into());
        if flow.is_selecting_train() {
            lines.extend([
                "↑↓ / jk Select a READY Train".into(),
                "Enter Continue to Service selection".into(),
                "Esc Cancel dispatch".into(),
            ]);
        } else if flow.is_selecting_service() {
            lines.extend([
                "↑↓ / jk Select a Passenger Service from this station".into(),
                "PgUp / PgDn Scroll longer route lists".into(),
                "Enter Review Journey".into(),
            ]);
            lines.push("← / Backspace Previous step   Esc Cancel".into());
        } else {
            lines.extend([
                "Enter Confirm dispatch".into(),
                "← / Backspace Previous step   Esc Cancel".into(),
            ]);
        }
        lines.extend([
            String::new(),
            "The footer always shows the actions available in the current step.".into(),
        ]);
        return lines;
    }

    if shell.active_view == View::BuyTrains {
        if let Some(flow) = &shell.market_flow {
            lines.push("Current · Train Purchase".into());
            if flow.is_selecting_delivery() {
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
            return lines;
        }
    }

    if shell.active_view == View::Trains && shell.fleet_flow.is_some() {
        lines.extend([
            "Current · Train Resale".into(),
            "Enter Confirm resale".into(),
            "Esc Cancel".into(),
        ]);
        return lines;
    }

    if shell.active_view == View::Company && shell.company_recovery_review_open {
        lines.extend([
            "Current · Financial Recovery".into(),
            "↑↓ / jk Select a recovery route".into(),
            "Enter Open the selected recovery action".into(),
            "Esc Back to Company".into(),
        ]);
        return lines;
    }

    if shell.active_view == View::Company && shell.company_receipt_details_open {
        lines.extend([
            "Current · Journey Receipt".into(),
            "Esc Back to Journey history".into(),
            "1–6 Switch workspace".into(),
        ]);
        return lines;
    }

    if shell.active_view == View::Map && shell.services_open {
        lines.push("Current · Passenger Services".into());
        if state.player_company.passenger_services.is_empty() {
            lines.extend([
                "n Create the first directional Passenger Service".into(),
                "Esc Return to Map".into(),
            ]);
        } else {
            lines.extend([
                "↑↓ / jk Select Passenger Service".into(),
                "PgUp / PgDn Move through longer Service lists".into(),
                "n Create a new directional Passenger Service".into(),
                "e Edit the selected Service when it has no active Journeys".into(),
                "d Delete the selected Service when it has no active Journeys".into(),
                "Esc Return to Map".into(),
            ]);
        }
        lines.extend([
            String::new(),
            "During create/edit: Enter adds a stop, Backspace removes the last stop, f reviews."
                .into(),
        ]);
        return lines;
    }

    match shell.active_view {
        View::Map => {
            let train_count = state.player_company.fleet.trains.len();
            let ready = state
                .player_company
                .fleet
                .trains
                .iter()
                .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
                .count();
            lines.extend([
                "Current · Map".into(),
                "↑↓←→ / hjkl Select a map location".into(),
                "s Open Passenger Services".into(),
                "w Open World Details".into(),
            ]);
            if train_count == 0 {
                lines.extend([
                    String::new(),
                    "Next step".into(),
                    "3 Open Market and acquire your first passenger Train".into(),
                ]);
            } else if ready == 0 {
                lines.extend([
                    "d Dispatch is unavailable while every Train is travelling".into(),
                    "Journeys continue while RailQ is closed; dispatch again after arrival".into(),
                ]);
            } else {
                lines.push(format!(
                    "d Manual Dispatch · {ready} READY {}",
                    if ready == 1 { "Train" } else { "Trains" }
                ));
            }
        }
        View::Trains => {
            lines.push("Current · Trains".into());
            if state.player_company.fleet.trains.is_empty() {
                lines.extend([
                    "No trains owned yet".into(),
                    String::new(),
                    "Next step".into(),
                    "3 Open Market and acquire your first passenger Train".into(),
                ]);
            } else if shell.fleet_details_open {
                lines.extend([
                    "Esc Back to Fleet".into(),
                    "r Rename selected Train".into(),
                    "d Dispatch selected READY Train".into(),
                    "s Review resale of selected READY Train".into(),
                ]);
            } else {
                lines.extend([
                    "↑↓ / jk Select Train".into(),
                    "PgUp / PgDn Scroll".into(),
                    "Enter Details".into(),
                    "r Rename selected Train".into(),
                    "d Dispatch selected READY Train".into(),
                    "s Review resale of selected READY Train".into(),
                ]);
            }
        }
        View::BuyTrains => {
            lines.extend([
                "Current · Market".into(),
                "↑↓ / jk Select Train model".into(),
                "Enter Buy selected Train".into(),
                String::new(),
                "Purchase price is not the whole decision: keep enough cash for access and fuel."
                    .into(),
            ]);
        }
        View::Company => {
            lines.push("Current · Company".into());
            lines.push("v Edit Company VKM".into());
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
        }
        View::Authority => {
            lines.extend([
                "Current · Rail Authority".into(),
                "↑↓ / jk Select infrastructure project".into(),
                "PgUp / PgDn Scroll project pipeline".into(),
                "f Contribute to selected project while it is in Funding".into(),
                String::new(),
                "The Authority controls public infrastructure; operator contributions are optional.".into(),
            ]);
        }
        View::Bulletin => {
            lines.extend([
                "Current · Railway Bulletin".into(),
                "↑↓ / jk Select development".into(),
                "PgUp / PgDn Scroll history".into(),
                "f Cycle Local / Authority / Construction / Network filters".into(),
                String::new(),
                "The Bulletin records significant world developments, not routine Train movements.".into(),
            ]);
        }
    }

    lines.extend([
        String::new(),
        "Tip".into(),
        "The footer is contextual: it only shows actions that matter right now.".into(),
    ]);
    lines
}

fn render_world_details_overlay(frame: &mut ratatui::Frame, area: Rect, state: &GameState) {
    let card = modal::centered_rect(area, 86, 24);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "World Details",
        modal::shortcut_line(&[("Esc/W", "close")]),
    );

    let region = &state.region;
    let registration = &region.railway_registration;
    let network = &region.rail_authority.rail_network;
    let connected = region
        .settlements
        .iter()
        .filter(|settlement| {
            network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
        })
        .count();
    let total = region.settlements.len();
    let coverage_percent = if total == 0 {
        0
    } else {
        connected.saturating_mul(100) / total
    };
    let network_metres = network.rail_lines.iter().fold(0_u64, |total, line| {
        total.saturating_add(line.distance.metres())
    });
    let territory = region
        .name
        .rsplit_once(" of ")
        .map_or(region.name.as_str(), |(_, territory)| territory);

    let lines = vec![
        world_section("REGION"),
        world_field("Region", &region.name),
        world_field("Rail Authority", &region.rail_authority.name),
        world_field("Population", &grouped_u64(region.population)),
        Line::from(""),
        world_section("RAILWAY REGISTRATION"),
        world_field(
            "Identity",
            &format!("{} · {}", registration.display_code(), registration.mark),
        ),
        world_field(
            &registration.display_code(),
            "fictional numeric railway registration code for this Region",
        ),
        world_field(
            &registration.mark,
            &format!("fictional two-letter railway mark assigned to {territory}"),
        ),
        Line::styled(
            "Used in official RailQ EVNs; it remains stable for this world.",
            theme::secondary(),
        ),
        Line::from(""),
        world_section("NETWORK"),
        world_field("Settlements", &format!("{total}")),
        world_field(
            "Connected",
            &format!("{connected} / {total} ({coverage_percent}%)"),
        ),
        world_field("Rail stations", &format!("{}", network.rail_stations.len())),
        world_field("Rail lines", &format!("{}", network.rail_lines.len())),
        world_field("Rail network", &format::distance(network_metres)),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        modal_areas.body,
    );
}

fn world_section(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::focused_title())
}

fn world_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn grouped_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

fn render_help_overlay(frame: &mut ratatui::Frame, area: Rect, shell: &Shell, state: &GameState) {
    let card = modal::centered_rect(area, 96, 30);
    let footer = if card.width < 76 {
        modal::shortcut_line(&[("↑↓", "scroll"), ("Esc/?", "close")])
    } else {
        modal::shortcut_line(&[
            ("↑↓", "scroll"),
            ("PgUp/PgDn", "page"),
            ("Esc/?", "close"),
            ("Q", "quit"),
        ])
    };
    let modal_areas = modal::render_shell(frame, card, "Keyboard Help", footer);

    let lines = help_lines(shell, state);
    let visible_lines = usize::from(modal_areas.body.height);
    let max_offset = lines.len().saturating_sub(visible_lines.max(1));
    let offset = shell.help_offset.min(max_offset);
    let content = lines
        .into_iter()
        .skip(offset)
        .take(visible_lines)
        .map(|line| {
            if line == "Navigation"
                || line == "Next step"
                || line == "Tip"
                || line.starts_with("Current ·")
            {
                Line::styled(line, theme::focused_title())
            } else {
                Line::from(line)
            }
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(content)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        modal_areas.body,
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
    lines.push(Line::styled("i / Esc · close details", theme::hint()));
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

fn render_bankruptcy_restart_confirmation(frame: &mut ratatui::Frame, area: Rect) {
    let card = modal::centered_rect(area, 70, 15);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "Confirm Safe Restart",
        modal::shortcut_line(&[("Enter", "restart"), ("Esc", "keep save")]),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Safe restart review", theme::focused_title()),
            Line::from(""),
            Line::from(
                "A fresh game is created only after the current Player Company save is preserved in a unique archive backup.",
            ),
            Line::from(""),
            Line::styled(
                "The existing save is never silently overwritten.",
                theme::warning(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

fn bankruptcy_text(restart_confirmation: bool) -> String {
    if restart_confirmation {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "",
            "Safe restart review is open. A fresh game is created only after this Player Company save is preserved in a unique archive backup.",
        ]
        .join("\n")
    } else {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "Normal operations are disabled. A safe restart preserves this Company save in an archive backup before creating a fresh game.",
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

fn pending_dispatch(state: &GameState, train_id: TrainId, service_id: ServiceId) -> PendingAction {
    let model: String = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or_else(
            || "unknown model".into(),
            |train| {
                model_for_train(train)
                    .map(|model| model.name().to_owned())
                    .unwrap_or_else(|| format!("unknown model ({})", train.model_id.as_str()))
            },
        );
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id);
    let service_name = service
        .map(|service| service.name.clone())
        .unwrap_or_else(|| format!("Service {}", service_id.get()));
    let route = service
        .map(|service| {
            service
                .stop_station_ids
                .iter()
                .map(|station_id| arrival_station_label(state, *station_id))
                .collect::<Vec<_>>()
                .join(" → ")
        })
        .unwrap_or_else(|| "unknown route".into());

    PendingAction {
        label: format!("Manual Dispatch · Train {:02} ({model})", train_id.get()),
        details: vec![format!(
            "{service_name} · {route}. Full operating costs are paid at departure; passenger revenue is credited stop by stop."
        )],
        funds_before: state.player_company.funds,
    }
}

fn pending_purchase(
    state: &GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
) -> PendingAction {
    let model: String = train_catalogue().models().get(catalogue_index).map_or_else(
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
        .map_or_else(
            || "unknown model".into(),
            |train| {
                model_for_train(train)
                    .map(|model| model.name().to_owned())
                    .unwrap_or_else(|| format!("unknown model ({})", train.model_id.as_str()))
            },
        );
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

    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
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

    use super::{
        Shell, ShellAction, View, capture_rendered_buffer, capture_rendered_cell_colors, theme,
    };

    #[test]
    fn routes_the_six_primary_views_by_number_and_keeps_letter_aliases() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));

        for (key, expected_view) in [
            ('2', View::Trains),
            ('4', View::Company),
            ('3', View::BuyTrains),
            ('1', View::Map),
            ('5', View::Authority),
            ('6', View::Bulletin),
            ('t', View::Trains),
            ('c', View::Company),
            ('b', View::BuyTrains),
            ('m', View::Map),
            ('a', View::Authority),
            ('u', View::Bulletin),
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
    fn map_world_details_explains_the_region_registration_identity() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert!(shell.world_details_visible);

        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        let registration = format!(
            "{} · {}",
            state.region.railway_registration.display_code(),
            state.region.railway_registration.mark
        );
        assert!(rendered.contains("World Details"));
        assert!(rendered.contains(&state.region.name));
        assert!(rendered.contains(&state.region.rail_authority.name));
        assert!(rendered.contains(&registration));
        assert!(rendered.contains("RAILWAY REGISTRATION"));
        assert!(rendered.contains("fictional two-letter railway mark"));
        assert!(rendered.contains("Connected"));
        assert!(rendered.contains("Rail network"));
        assert!(rendered.contains("[Esc/W] close"));
        assert!(!rendered.contains("w / Esc · return to Map"));
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
            Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
            "World Details should mute the application underneath it",
        );
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 17, 8),
            Some((theme::ACCENT, theme::PANEL)),
            "World Details should use the shared focused-modal border",
        );

        assert_eq!(
            shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state),
            ShellAction::Continue
        );
        assert!(!shell.world_details_visible);
    }

    #[test]
    fn company_vkm_editor_normalizes_input_and_emits_a_persisted_action() {
        let mut shell = Shell::new();
        let state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));

        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert_eq!(
            shell.handle_key(
                KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
                &state,
            ),
            ShellAction::Continue
        );
        assert!(shell.company_vkm_editor.is_some());
        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(rendered.contains("Edit Vehicle Keeper Mark"));
        assert!(rendered.contains("VEHICLE KEEPER MARK"));
        assert!(rendered.contains("[Enter] save"));
        assert!(rendered.contains("[Backspace] delete"));
        assert!(rendered.contains("[Esc] cancel"));
        assert_eq!(
            capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
            Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
            "VKM editor should mute the Company dashboard underneath it",
        );

        for _ in 0..3 {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                    &state,
                ),
                ShellAction::Continue
            );
        }
        for character in ['o', 'm', 'p'] {
            assert_eq!(
                shell.handle_key(
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    &state,
                ),
                ShellAction::Continue
            );
        }

        let action = shell.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &state);
        match action {
            ShellAction::UpdateCompanyVkm {
                vehicle_keeper_mark,
            } => {
                assert_eq!(vehicle_keeper_mark.as_str(), "OMP");
            }
            other => panic!("unexpected action: {other:?}"),
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
        assert!(wide.contains("ETA"));
        assert!(wide.contains("1 Map"));
        assert!(wide.contains("2 Trains"));
        assert!(wide.contains("q Quit"));

        let compact = capture_rendered_buffer(&shell, &state, 80, 24);
        assert!(compact.contains(&format!("Funds {funds}")));
        assert!(compact.contains("R 0"));
        assert!(compact.contains("T 1"));
        assert!(compact.contains("ETA"));
        assert!(compact.contains("4 Co"));
        assert!(compact.contains("3 Mkt"));
        assert!(compact.contains("q Quit"));

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
        assert!(feedback_lines[39].contains("q Quit"));

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

        let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
        assert!(rendered.contains("Trains · 2 total · 2 READY · 0 TRAVELLING"));
        assert!(rendered.contains("OPERATIONS"));
        assert!(rendered.contains("SPECIFICATIONS"));
        assert!(rendered.contains("D Dispatch"));
        assert!(!rendered.contains("Enter Inspect"));
    }

    #[test]
    fn map_routes_a_keyboard_manual_dispatch_to_the_application_boundary() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service_id =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(3))
                .unwrap();
        let mut shell = Shell::new();
        let press = |shell: &mut Shell, key| {
            shell.handle_key(KeyEvent::new(key, KeyModifiers::NONE), &state)
        };

        assert_eq!(press(&mut shell, KeyCode::Char('d')), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(press(&mut shell, KeyCode::Enter), ShellAction::Continue);
        assert_eq!(
            press(&mut shell, KeyCode::Enter),
            ShellAction::ManualDispatch {
                train_id,
                service_id,
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
    fn help_is_contextual_and_points_a_new_company_to_the_market() {
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
        let help = super::help_lines(&shell, &state).join("\n");
        for instruction in [
            "1 Map",
            "2 Trains",
            "3 Market",
            "4 Company",
            "Current · Map",
            "3 Open Market and acquire your first passenger Train",
        ] {
            assert!(help.contains(instruction));
        }
        shell.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &state);
        assert!(!shell.help_visible());
    }

    #[test]
    fn help_changes_with_the_active_workspace() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let mut shell = Shell::new();

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
            &state,
        );
        let trains_help = super::help_lines(&shell, &state).join("\n");
        assert!(trains_help.contains("Current · Trains"));
        assert!(trains_help.contains("d Dispatch selected READY Train"));
        assert!(trains_help.contains("r Rename selected Train"));

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
            &state,
        );
        let market_help = super::help_lines(&shell, &state).join("\n");
        assert!(market_help.contains("Current · Market"));
        assert!(market_help.contains("Enter Buy selected Train"));

        shell.handle_key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &state,
        );
        shell.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &state,
        );
        let services_help = super::help_lines(&shell, &state).join("\n");
        assert!(services_help.contains("Current · Passenger Services"));
        assert!(services_help.contains("n Create the first directional Passenger Service"));
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
