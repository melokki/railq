//! Terminal shell and shared navigation state.
//!
//! This module deliberately owns terminal I/O only. Game actions remain at the
//! application boundary, where callers supply an elapsed-time reconciliation
//! callback before the shell accepts each input event.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crate::{
    app::{AppCommand, AppCommandResult},
    model::{GameState, Money},
    sim::{
        finance::{FinancialStatus, evaluate_financial_recovery},
        time::SettledJourney,
    },
};

pub mod authority;
pub mod bulletin;
pub mod company;
pub mod components;
pub mod dispatch;
pub mod fleet;
pub mod format;
pub mod layout;
pub mod map;
pub mod market;
pub mod modal;
mod chrome;
use chrome::{HELP_PAGE_STEP, help_lines};
mod overlays;
use overlays::ActionOutcome;
mod feedback;
use feedback::{PendingAction, arrival_station_label, pending_dispatch, pending_purchase, pending_resale};
mod shell_render;
use shell_render::render_frame;
mod runtime;
pub use runtime::{
    RunError, TerminalCommand, TerminalCommandOutcome, run_terminal, run_terminal_with_arrivals,
};
pub mod services;
pub mod start;
pub mod theme;

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
    /// Execute one presentation-independent player command.
    Player(AppCommand),
    /// Confirmed Bankruptcy restart requiring an archived-save application action.
    RestartAfterBankruptcy,
}

/// Presentation-only state shared by the six primary views.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    active_view: View,
    dispatch_workspace: dispatch::DispatchWorkspace,
    map_workspace: map::MapWorkspace,
    service_workspace: services::ServiceWorkspace,
    fleet_workspace: fleet::FleetWorkspace,
    market_workspace: market::MarketWorkspace,
    authority_workspace: authority::AuthorityWorkspace,
    bulletin_workspace: bulletin::BulletinWorkspace,
    company_workspace: company::CompanyWorkspace,
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
            dispatch_workspace: dispatch::DispatchWorkspace::default(),
            map_workspace: map::MapWorkspace::default(),
            service_workspace: services::ServiceWorkspace::default(),
            fleet_workspace: fleet::FleetWorkspace::default(),
            market_workspace: market::MarketWorkspace::default(),
            authority_workspace: authority::AuthorityWorkspace::default(),
            bulletin_workspace: bulletin::BulletinWorkspace::default(),
            company_workspace: company::CompanyWorkspace::default(),
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

        if self.fleet_workspace.has_nickname_editor() {
            let action = self.fleet_workspace.handle_nickname_key(key.code);
            return self.handle_fleet_workspace_action(action, state);
        }

        if self.fleet_workspace.has_assignment_flow() {
            let action = self.fleet_workspace.handle_assignment_key(key.code, state);
            return self.handle_fleet_workspace_action(action, state);
        }

        if self.company_workspace.has_vkm_editor() {
            return self.handle_company_key(key, state);
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

        if self.map_workspace.world_details_visible() {
            match self.map_workspace.handle_world_details_key(key.code) {
                map::WorldDetailsKeyAction::Continue => return ShellAction::Continue,
                map::WorldDetailsKeyAction::Closed => {
                    self.notice = None;
                    return ShellAction::Continue;
                }
                map::WorldDetailsKeyAction::ClosedForNavigation => {
                    self.notice = None;
                }
            }
        }

        if self.map_workspace.movements_visible() {
            match self.map_workspace.handle_movements_key(key.code) {
                map::MovementsKeyAction::Continue => return ShellAction::Continue,
                map::MovementsKeyAction::Closed => {
                    self.notice = None;
                    return ShellAction::Continue;
                }
                map::MovementsKeyAction::ClosedForNavigation => {
                    self.notice = None;
                }
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

        if self.authority_workspace.has_modal() {
            return self.handle_authority_key(key, state);
        }

        if let Some(flow) = self.dispatch_workspace.flow_mut() {
            return match flow.handle_key(key, state) {
                dispatch::DispatchFlowAction::Continue => ShellAction::Continue,
                dispatch::DispatchFlowAction::Cancel => {
                    if self.dispatch_workspace.close() {
                        self.fleet_workspace.close_details();
                    }
                    self.notice = Some("Manual Dispatch cancelled; no changes were made.".into());
                    ShellAction::Continue
                }
                dispatch::DispatchFlowAction::Confirm {
                    train_id,
                    service_id,
                } => {
                    self.pending_action = Some(pending_dispatch(state, train_id, service_id));
                    ShellAction::Player(AppCommand::ManualDispatch {
                        train_id,
                        service_id,
                    })
                }
            };
        }

        if self.fleet_workspace.has_resale_flow() {
            let action = self.fleet_workspace.handle_resale_key(key, state);
            return self.handle_fleet_workspace_action(action, state);
        }

        if self.market_workspace.has_flow() {
            return self.handle_market_key(key, state);
        }

        if self.service_workspace.is_open() {
            let navigation_key = matches!(
                key.code,
                KeyCode::Char(
                    '1' | '2' | '3' | '4' | '5' | '6' | 't' | 'T' | 'b' | 'B' | 'c' | 'C' | 'u' | 'U'
                )
            );
            if navigation_key {
                self.service_workspace.close();
            } else {
                return match self.service_workspace.handle_key(key.code, state) {
                    services::ServiceWorkspaceAction::Continue => ShellAction::Continue,
                    services::ServiceWorkspaceAction::Close => {
                        self.service_workspace.close();
                        self.notice = None;
                        ShellAction::Continue
                    }
                    services::ServiceWorkspaceAction::RunService { service_id } => {
                        match self.dispatch_workspace.start_from_service(state, service_id) {
                            Ok(()) => self.notice = None,
                            Err(message) => self.notice = Some(message),
                        }
                        ShellAction::Continue
                    }
                    services::ServiceWorkspaceAction::Create { stop_station_ids } => {
                        ShellAction::Player(AppCommand::CreatePassengerService { stop_station_ids })
                    }
                    services::ServiceWorkspaceAction::Update {
                        service_id,
                        stop_station_ids,
                    } => ShellAction::Player(AppCommand::UpdatePassengerService {
                        service_id,
                        stop_station_ids,
                    }),
                    services::ServiceWorkspaceAction::Delete { service_id } => {
                        ShellAction::Player(AppCommand::DeletePassengerService { service_id })
                    }
                    services::ServiceWorkspaceAction::AssignTrain { train_id, service_id } => {
                        ShellAction::Player(AppCommand::AssignTrainToService {
                            train_id,
                            service_id,
                        })
                    }
                    services::ServiceWorkspaceAction::UnassignTrain { train_id } => {
                        ShellAction::Player(AppCommand::UnassignTrainFromService { train_id })
                    }
                };
            }
        }

        if self.company_workspace.recovery_review_open() {
            return self.handle_company_key(key, state);
        }

        match key.code {
            KeyCode::Char('1') => {
                self.active_view = View::Map;
                self.service_workspace.close();
            }
            KeyCode::Char('2' | 't' | 'T') => {
                self.active_view = View::Trains;
                self.service_workspace.close();
                self.fleet_workspace.activate();
            }
            KeyCode::Char('4' | 'c' | 'C') => {
                self.active_view = View::Company;
                self.service_workspace.close();
                self.company_workspace.activate();
            }
            KeyCode::Char('a' | 'A') if self.active_view == View::Trains => {
                let action = self.fleet_workspace.handle_key(key, state);
                return self.handle_fleet_workspace_action(action, state);
            }
            KeyCode::Char('3' | 'b' | 'B') => {
                self.active_view = View::BuyTrains;
                self.service_workspace.close();
            }
            KeyCode::Char('5' | 'a' | 'A') => {
                self.active_view = View::Authority;
                self.service_workspace.close();
            }
            KeyCode::Char('6' | 'u' | 'U') => {
                self.active_view = View::Bulletin;
                self.service_workspace.close();
            }
            KeyCode::Enter
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::BuyTrains =>
            {
                return self.handle_market_key(key, state);
            }
            KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('r' | 'R' | 'n' | 'N' | 's' | 'S' | 'd' | 'D')
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Trains =>
            {
                let action = self.fleet_workspace.handle_key(key, state);
                return self.handle_fleet_workspace_action(action, state);
            }
            KeyCode::Char('f' | 'F') if self.active_view == View::Authority => {
                return self.handle_authority_key(key, state);
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K')
                if self.active_view == View::Authority =>
            {
                return self.handle_authority_key(key, state);
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
            _ if self.active_view == View::Map => {
                return self.handle_map_key(key, state);
            }
            _ if self.active_view == View::Company => {
                return self.handle_company_key(key, state);
            }
            _ => {}
        }
        ShellAction::Continue
    }

    fn handle_map_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        match self.map_workspace.handle_key(key.code, state) {
            map::MapWorkspaceAction::Continue => ShellAction::Continue,
            map::MapWorkspaceAction::ClearNotice => {
                self.notice = None;
                ShellAction::Continue
            }
            map::MapWorkspaceAction::OpenServices => {
                self.service_workspace.open();
                self.notice = None;
                ShellAction::Continue
            }
            map::MapWorkspaceAction::StartDispatch => {
                match self.dispatch_workspace.start_from_map(state) {
                    Ok(()) => {
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message.into()),
                }
                ShellAction::Continue
            }
        }
    }

    fn handle_company_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        match self.company_workspace.handle_key(key, state) {
            company::CompanyWorkspaceAction::Continue => ShellAction::Continue,
            company::CompanyWorkspaceAction::ClearNotice => {
                self.notice = None;
                ShellAction::Continue
            }
            company::CompanyWorkspaceAction::Notice(message) => {
                self.notice = Some(message);
                ShellAction::Continue
            }
            company::CompanyWorkspaceAction::Navigate(destination) => {
                self.active_view = match destination {
                    company::RecoveryDestination::Fleet => View::Trains,
                    company::RecoveryDestination::BuyTrains => View::BuyTrains,
                    company::RecoveryDestination::Map => View::Map,
                };
                self.notice = Some(
                    "Recovery route opened for review only; no action has been authorised."
                        .into(),
                );
                ShellAction::Continue
            }
            company::CompanyWorkspaceAction::UpdateVkm(vehicle_keeper_mark) => {
                ShellAction::Player(AppCommand::UpdateCompanyVkm {
                    vehicle_keeper_mark,
                })
            }
        }
    }

    fn handle_market_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        match self.market_workspace.handle_key(key, state) {
            market::MarketWorkspaceAction::Continue => ShellAction::Continue,
            market::MarketWorkspaceAction::ClearNotice => {
                self.notice = None;
                ShellAction::Continue
            }
            market::MarketWorkspaceAction::Notice(message) => {
                self.notice = Some(message);
                ShellAction::Continue
            }
            market::MarketWorkspaceAction::Purchase {
                catalogue_index,
                delivery_station_id,
            } => {
                self.pending_action = Some(pending_purchase(
                    state,
                    catalogue_index,
                    delivery_station_id,
                ));
                ShellAction::Player(AppCommand::PurchaseTrain {
                    catalogue_index,
                    delivery_station_id,
                })
            }
        }
    }

    fn handle_authority_key(&mut self, key: KeyEvent, state: &GameState) -> ShellAction {
        match self.authority_workspace.handle_key(key, state) {
            authority::AuthorityWorkspaceAction::Continue => ShellAction::Continue,
            authority::AuthorityWorkspaceAction::ClearNotice => {
                self.notice = None;
                ShellAction::Continue
            }
            authority::AuthorityWorkspaceAction::Notice(message) => {
                self.notice = Some(message);
                ShellAction::Continue
            }
            authority::AuthorityWorkspaceAction::Contribute { project_id, amount } => {
                ShellAction::Player(AppCommand::ContributeInfrastructure { project_id, amount })
            }
        }
    }

    fn handle_fleet_workspace_action(
        &mut self,
        action: fleet::FleetWorkspaceAction,
        state: &GameState,
    ) -> ShellAction {
        match action {
            fleet::FleetWorkspaceAction::Continue => ShellAction::Continue,
            fleet::FleetWorkspaceAction::ClearNotice => {
                self.notice = None;
                ShellAction::Continue
            }
            fleet::FleetWorkspaceAction::Notice(message) => {
                self.notice = Some(message);
                ShellAction::Continue
            }
            fleet::FleetWorkspaceAction::SellTrain { train_id } => {
                self.pending_action = Some(pending_resale(state, train_id));
                ShellAction::Player(AppCommand::SellTrain { train_id })
            }
            fleet::FleetWorkspaceAction::UpdateNickname { train_id, nickname } => {
                ShellAction::Player(AppCommand::UpdateTrainNickname { train_id, nickname })
            }
            fleet::FleetWorkspaceAction::Dispatch { train_id } => {
                match self.dispatch_workspace.start_from_fleet(state, train_id) {
                    Ok(()) => {
                        self.notice = None;
                    }
                    Err(message) => self.notice = Some(message),
                }
                ShellAction::Continue
            }
            fleet::FleetWorkspaceAction::AssignService { train_id, service_id } => {
                ShellAction::Player(AppCommand::AssignTrainToService { train_id, service_id })
            }
            fleet::FleetWorkspaceAction::UnassignService { train_id } => {
                ShellAction::Player(AppCommand::UnassignTrainFromService { train_id })
            }
        }
    }

    /// Keeps a rejected confirmation visible to explain the actual current-state cause.
    pub fn reject_manual_dispatch(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(flow) = self.dispatch_workspace.flow_mut() {
            flow.reject(error);
        } else {
            self.notice = Some(error.into());
        }
    }

    /// Closes a successful proposal after the application boundary persisted it.
    pub fn confirm_manual_dispatch(&mut self) {
        self.pending_action = None;
        if self.dispatch_workspace.close() {
            self.fleet_workspace.close_details();
        }
        self.notice = Some("Manual Dispatch authorised and saved.".into());
    }

    /// Publishes dispatch feedback only after the caller has saved and supplied
    /// the resulting game state.
    pub fn confirm_manual_dispatch_saved(&mut self, state: &GameState) {
        self.dispatch_workspace.reset();
        self.publish_pending_outcome(state);
    }

    /// Publishes UI feedback from the typed result of a durably saved player command.
    fn confirm_player_command_saved(&mut self, result: &AppCommandResult, state: &GameState) {
        match result {
            AppCommandResult::JourneyDispatched { .. } => self.confirm_manual_dispatch_saved(state),
            AppCommandResult::TrainPurchased { .. } => self.confirm_purchase_train_saved(state),
            AppCommandResult::TrainSold { .. } => self.confirm_train_resale_saved(state),
            AppCommandResult::PassengerServiceCreated { .. } => {
                self.confirm_passenger_service_created(state)
            }
            AppCommandResult::PassengerServiceUpdated { .. } => {
                self.confirm_passenger_service_updated(state)
            }
            AppCommandResult::PassengerServiceDeleted { .. } => {
                self.confirm_passenger_service_deleted(state)
            }
            AppCommandResult::TrainServiceAssigned {
                train_id,
                service_id,
            } => self.confirm_train_service_assignment_saved(*train_id, Some(*service_id)),
            AppCommandResult::TrainServiceUnassigned { train_id } => {
                self.confirm_train_service_assignment_saved(*train_id, None)
            }
            AppCommandResult::CompanyVkmUpdated => self.confirm_company_vkm_saved(state),
            AppCommandResult::InfrastructureContributionRecorded { .. } => {
                self.confirm_infrastructure_contribution_saved(state)
            }
            AppCommandResult::TrainNicknameUpdated { .. } => {
                self.confirm_train_nickname_saved(state)
            }
        }
    }

    fn reject_player_command(&mut self, command: &AppCommand, error: String) {
        match command {
            AppCommand::ManualDispatch { .. } => self.reject_manual_dispatch(error),
            AppCommand::PurchaseTrain { .. } => self.reject_purchase_train(error),
            AppCommand::SellTrain { .. } => self.reject_train_resale(error),
            AppCommand::CreatePassengerService { .. }
            | AppCommand::UpdatePassengerService { .. }
            | AppCommand::DeletePassengerService { .. } => {
                self.reject_passenger_service_action(error)
            }
            AppCommand::AssignTrainToService { .. }
            | AppCommand::UnassignTrainFromService { .. } => {
                if self.service_workspace.reject_assignment(error.clone()).is_none() {
                    self.notice = None;
                } else if let Some(message) = self.fleet_workspace.reject_assignment(error.clone()) {
                    self.notice = Some(message);
                } else {
                    self.notice = Some(error);
                }
            }
            AppCommand::UpdateCompanyVkm { .. } => self.reject_company_vkm_update(error),
            AppCommand::ContributeInfrastructure { .. } => {
                self.reject_infrastructure_contribution(error)
            }
            AppCommand::UpdateTrainNickname { .. } => self.reject_train_nickname_update(error),
        }
    }

    pub fn reject_purchase_train(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(message) = self.market_workspace.reject(error) {
            self.notice = Some(message);
        }
    }

    /// Closes a successful purchase proposal after the application boundary persisted it.
    pub fn confirm_purchase_train(&mut self) {
        self.pending_action = None;
        self.market_workspace.confirm_saved();
        self.notice = Some("Train purchase authorised and saved to the Fleet.".into());
    }

    /// Publishes purchase feedback only after the save boundary succeeded.
    pub fn confirm_purchase_train_saved(&mut self, state: &GameState) {
        self.market_workspace.confirm_saved();
        self.publish_pending_outcome(state);
    }

    /// Keeps a rejected resale visible to explain the actual current-state cause.
    pub fn reject_train_resale(&mut self, error: impl Into<String>) {
        self.pending_action = None;
        if let Some(message) = self.fleet_workspace.reject_resale(error) {
            self.notice = Some(message);
        }
    }

    /// Closes a saved resale and shows the actual proceeds credited to Company Funds.
    pub fn confirm_train_resale(&mut self, proceeds: crate::model::Money) {
        self.pending_action = None;
        self.fleet_workspace.confirm_resale_saved();
        self.notice = Some(format!(
            "Train resold and saved. Sale proceeds of {} were added to Company Funds.",
            format_money(proceeds)
        ));
    }

    /// Publishes resale feedback only after the save boundary succeeded.
    pub fn confirm_train_resale_saved(&mut self, state: &GameState) {
        self.fleet_workspace.confirm_resale_saved();
        self.publish_pending_outcome(state);
    }

    pub fn reject_infrastructure_contribution(&mut self, error: impl Into<String>) {
        self.notice = Some(error.into());
    }

    pub fn confirm_infrastructure_contribution_saved(&mut self, state: &GameState) {
        let amount = self.authority_workspace.confirm_saved();
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
        self.notice = Some("Passenger Service created and saved.".into());
    }

    /// Closes a saved Passenger Service edit and keeps the Services workspace open.
    pub fn confirm_passenger_service_updated(&mut self, state: &GameState) {
        self.service_workspace.confirm_updated(state);
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
        self.notice = Some("Passenger Service deleted and saved.".into());
    }

    /// Closes a saved Train-to-Service assignment change.
    pub fn confirm_train_service_assignment_saved(
        &mut self,
        train_id: crate::model::TrainId,
        service_id: Option<crate::model::ServiceId>,
    ) {
        self.fleet_workspace.confirm_assignment_saved();
        self.service_workspace.confirm_assignment_saved();
        self.notice = Some(match service_id {
            Some(service_id) => format!(
                "Train {:02} assigned to R{} and saved.",
                train_id.get(),
                service_id.get()
            ),
            None => format!("Train {:02} unassigned and saved.", train_id.get()),
        });
    }

    /// Closes the Company VKM editor after a persisted update.
    pub fn confirm_company_vkm_saved(&mut self, state: &GameState) {
        self.company_workspace.confirm_vkm_saved();
        self.notice = Some(format!(
            "Company VKM updated and saved as {}.",
            state.player_company.vehicle_keeper_mark
        ));
    }

    /// Closes a saved Train nickname edit and reports its current label.
    pub fn confirm_train_nickname_saved(&mut self, state: &GameState) {
        let train_id = self.fleet_workspace.confirm_nickname_saved();
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
        self.dispatch_workspace.reset();
        self.fleet_workspace.reset();
        self.market_workspace.reset();
        self.authority_workspace.reset();
        self.map_workspace.reset();
        self.service_workspace = services::ServiceWorkspace::default();
        self.restart_confirmation = false;
        self.company_workspace.reset();
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

pub mod testing;
pub use testing::{
    capture_rendered_buffer, capture_rendered_buffer_mut, capture_rendered_cell_colors,
};

fn is_bankrupt(state: &GameState) -> bool {
    matches!(
        evaluate_financial_recovery(state),
        Ok(evaluation) if evaluation.status == FinancialStatus::Bankruptcy
    )
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

#[cfg(test)]
mod tests;
