//! Application-boundary orchestration for persisted RailQ commands.
//!
//! Simulation transitions operate on a candidate [`GameState`]. The candidate
//! is written before it replaces the state visible to the rest of the app, so
//! a failed save never publishes a partial Player Company action.

pub mod command;

pub use command::{AppCommand, AppCommandResult};

use std::{error::Error, fmt, path::PathBuf};

use crate::{
    model::{
        GameState, InfrastructureProjectId, Money, RailStationId, ServiceDirectionMode, ServiceId,
        TrainId, TrainNickname, TrainStatus, UtcSeconds, VehicleKeeperMark,
    },
    sim::{
        authority::{InfrastructureProjectActionError, contribute_to_infrastructure_project},
        economy::EconomyError,
        finance::{FinanceError, FinancialStatus, evaluate_financial_recovery},
        fleet::{FleetError, purchase_train, sell_train},
        journeys::{DispatchError, dispatch_journey, dispatch_positioning_journey},
        services::{
            ServiceAssignmentError, ServiceError, assign_train_to_service,
            create_service_with_mode, delete_service, find_or_create_service, rename_service,
            unassign_train_from_service, update_service_with_mode,
        },
        time::{AdvanceTimeError, SettledJourney, advance_time, advance_time_with_arrivals},
        world::create_new_game,
    },
    storage::{SaveSlot, SaveSlotError},
};

/// The persistence boundary used by the application command coordinator.
///
/// Production uses [`SaveSlot`]. Keeping this narrow permits deterministic
/// failure tests without putting test-only failure switches in disk storage.
pub trait GameStore {
    type Error;

    /// Returns a validated persisted state, or `None` when onboarding has not
    /// yet created a Player Company.
    fn load(&self) -> Result<Option<GameState>, Self::Error>;

    /// Durably replaces the persisted state.
    fn save(&self, state: &GameState) -> Result<(), Self::Error>;
}

impl GameStore for SaveSlot {
    type Error = SaveSlotError;

    fn load(&self) -> Result<Option<GameState>, Self::Error> {
        SaveSlot::load(self)
    }

    fn save(&self, state: &GameState) -> Result<(), Self::Error> {
        SaveSlot::save(self, state)
    }
}

/// A recoverable error at the command and persistence boundary.
#[derive(Debug)]
pub enum AppError<E> {
    /// The save could not be read during reload.
    Load(E),
    /// The candidate state could not be persisted, so it was not published.
    Save(E),
    /// Operating-state reconciliation could not form a valid candidate.
    Advance(AdvanceTimeError),
    /// A Train purchase was rejected by the simulation.
    Purchase(FleetError),
    /// A Train resale was rejected by the simulation.
    Resale(FleetError),
    /// A Train nickname update was rejected by the simulation.
    Rename(FleetError),
    /// A Manual Dispatch was rejected by the simulation.
    Dispatch(DispatchError),
    /// A Passenger Service could not be created or reused for a Manual Dispatch.
    Service(ServiceError),
    /// A Train-to-Service allocation could not be changed.
    ServiceAssignment(ServiceAssignmentError),
    /// The financial recovery evaluator could not determine whether operations remain allowed.
    Finance(FinanceError),
    /// A Player Company infrastructure contribution was rejected.
    Authority(InfrastructureProjectActionError),
    /// Bankruptcy prevents purchases, resale, and Manual Dispatch.
    Bankruptcy,
    /// A safe restart was requested before the Player Company reached Bankruptcy.
    RestartUnavailable { status: FinancialStatus },
}

impl<E: fmt::Display> fmt::Display for AppError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(formatter, "could not load saved game: {error}"),
            Self::Save(error) => write!(formatter, "could not save game changes: {error}"),
            Self::Advance(error) => error.fmt(formatter),
            Self::Purchase(error) => error.fmt(formatter),
            Self::Resale(error) => error.fmt(formatter),
            Self::Rename(error) => error.fmt(formatter),
            Self::Dispatch(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
            Self::ServiceAssignment(error) => error.fmt(formatter),
            Self::Finance(error) => error.fmt(formatter),
            Self::Authority(error) => error.fmt(formatter),
            Self::Bankruptcy => write!(
                formatter,
                "Bankruptcy prevents normal operations; exit or start a confirmed safe restart"
            ),
            Self::RestartUnavailable { status } => write!(
                formatter,
                "safe restart is only available after Bankruptcy (current financial status: {status:?})"
            ),
        }
    }
}

impl<E: Error + 'static> Error for AppError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) | Self::Save(error) => Some(error),
            Self::Advance(error) => Some(error),
            Self::Service(error) => Some(error),
            Self::ServiceAssignment(error) => Some(error),
            Self::Finance(error) => Some(error),
            Self::Authority(error) => Some(error),
            Self::Purchase(error) | Self::Resale(error) | Self::Rename(error) => Some(error),
            Self::Dispatch(error) => Some(error),
            Self::Bankruptcy | Self::RestartUnavailable { .. } => None,
        }
    }
}

/// The published game state and its exclusively owned persistence boundary.
#[derive(Debug)]
pub struct App<S> {
    state: GameState,
    store: S,
}

/// A loaded application together with arrivals durably settled during loading.
#[derive(Debug)]
pub struct LoadedApp<S> {
    app: App<S>,
    settled_arrivals: Vec<SettledJourney>,
}

impl<S> LoadedApp<S> {
    /// Separates the playable application from its one-time startup outcomes.
    pub fn into_parts(self) -> (App<S>, Vec<SettledJourney>) {
        (self.app, self.settled_arrivals)
    }
}

impl<S: GameStore> App<S> {
    /// Executes one presentation-independent application command at `now`.
    ///
    /// This is the migration seam for moving command routing out of the
    /// terminal UI. Existing focused methods remain available while commands
    /// are migrated incrementally.
    pub fn execute(
        &mut self,
        command: AppCommand,
        now: UtcSeconds,
    ) -> Result<AppCommandResult, AppError<S::Error>> {
        match command {
            AppCommand::PurchaseTrain {
                catalogue_index,
                delivery_station_id,
            } => {
                let train_id = self.purchase_train(catalogue_index, delivery_station_id, now)?;
                Ok(AppCommandResult::TrainPurchased { train_id })
            }
            AppCommand::SellTrain { train_id } => {
                let proceeds = self.sell_train(train_id, now)?;
                Ok(AppCommandResult::TrainSold { train_id, proceeds })
            }
            AppCommand::CreatePassengerService {
                stop_station_ids,
                direction_mode,
            } => {
                let service_id =
                    self.create_passenger_service_with_mode(stop_station_ids, direction_mode, now)?;
                Ok(AppCommandResult::PassengerServiceCreated { service_id })
            }
            AppCommand::UpdatePassengerService {
                service_id,
                stop_station_ids,
                direction_mode,
            } => {
                self.update_passenger_service_with_mode(
                    service_id,
                    stop_station_ids,
                    direction_mode,
                    now,
                )?;
                Ok(AppCommandResult::PassengerServiceUpdated { service_id })
            }
            AppCommand::UpdatePassengerServiceName {
                service_id,
                custom_name,
            } => {
                self.update_passenger_service_name(service_id, custom_name, now)?;
                Ok(AppCommandResult::PassengerServiceRenamed { service_id })
            }
            AppCommand::DeletePassengerService { service_id } => {
                self.delete_passenger_service(service_id, now)?;
                Ok(AppCommandResult::PassengerServiceDeleted { service_id })
            }
            AppCommand::AssignTrainToService {
                train_id,
                service_id,
            } => {
                self.assign_train_to_passenger_service(train_id, service_id, now)?;
                Ok(AppCommandResult::TrainServiceAssigned {
                    train_id,
                    service_id,
                })
            }
            AppCommand::UnassignTrainFromService { train_id } => {
                self.unassign_train_from_passenger_service(train_id, now)?;
                Ok(AppCommandResult::TrainServiceUnassigned { train_id })
            }
            AppCommand::ManualDispatch {
                train_id,
                service_id,
            } => {
                let journey_id = self.dispatch_journey(train_id, service_id, now)?;
                Ok(AppCommandResult::JourneyDispatched { journey_id })
            }
            AppCommand::PositionTrainForService {
                train_id,
                service_id,
                destination_station_id,
            } => {
                let journey_id = self.position_train_for_service(
                    train_id,
                    service_id,
                    destination_station_id,
                    now,
                )?;
                Ok(AppCommandResult::TrainPositioningStarted { journey_id })
            }
            AppCommand::ContributeInfrastructure { project_id, amount } => {
                self.contribute_to_infrastructure_project(project_id, amount, now)?;
                Ok(AppCommandResult::InfrastructureContributionRecorded { project_id, amount })
            }
            AppCommand::UpdateCompanyVkm {
                vehicle_keeper_mark,
            } => {
                self.update_company_vkm(vehicle_keeper_mark, now)?;
                Ok(AppCommandResult::CompanyVkmUpdated)
            }
            AppCommand::UpdateTrainNickname { train_id, nickname } => {
                self.update_train_nickname(train_id, nickname, now)?;
                Ok(AppCommandResult::TrainNicknameUpdated { train_id })
            }
        }
    }

    /// Persists a freshly created Player Company before allowing play.
    pub fn start_new(store: S, state: GameState) -> Result<Self, AppError<S::Error>> {
        store.save(&state).map_err(AppError::Save)?;
        Ok(Self { state, store })
    }

    /// Loads a save, reconciles elapsed Journeys, and persists that result
    /// before exposing it to the caller. A missing save remains an onboarding
    /// condition rather than a silently generated fresh game.
    pub fn load(store: S, now: UtcSeconds) -> Result<Option<Self>, AppError<S::Error>> {
        Ok(Self::load_or_empty(store, now)?
            .ok()
            .map(|loaded| loaded.into_parts().0))
    }

    /// Loads a saved Player Company or returns the still-owned empty store for
    /// onboarding. This keeps the exclusive save-slot lock open between a
    /// missing-save check and the first persisted Player Company.
    pub fn load_or_empty(
        store: S,
        now: UtcSeconds,
    ) -> Result<Result<LoadedApp<S>, S>, AppError<S::Error>> {
        let Some(mut state) = store.load().map_err(AppError::Load)? else {
            return Ok(Err(store));
        };
        let settled_arrivals =
            advance_time_with_arrivals(&mut state, now).map_err(AppError::Advance)?;
        store.save(&state).map_err(AppError::Save)?;
        Ok(Ok(LoadedApp {
            app: Self { state, store },
            settled_arrivals,
        }))
    }

    /// Returns the last successfully persisted, published game state.
    pub fn state(&self) -> &GameState {
        &self.state
    }

    /// Buys and delivers a Train only if the advanced candidate can be saved.
    pub fn purchase_train(
        &mut self,
        catalogue_index: usize,
        delivery_station_id: RailStationId,
        now: UtcSeconds,
    ) -> Result<TrainId, AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            let train_id = purchase_train(state, catalogue_index, delivery_station_id)
                .map_err(AppError::Purchase)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(train_id)
            }
        })
    }

    /// Sells a READY Train only if the advanced candidate can be saved.
    pub fn sell_train(
        &mut self,
        train_id: TrainId,
        now: UtcSeconds,
    ) -> Result<crate::model::Money, AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            let proceeds = sell_train(state, train_id).map_err(AppError::Resale)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(proceeds)
            }
        })
    }

    /// Contributes Player Company cash to one Authority project in Funding.
    pub fn contribute_to_infrastructure_project(
        &mut self,
        project_id: InfrastructureProjectId,
        amount: Money,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, now| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            contribute_to_infrastructure_project(state, project_id, amount, now)
                .map_err(AppError::Authority)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(())
            }
        })
    }

    /// Creates and persists one bidirectional Passenger Service.
    pub fn create_passenger_service(
        &mut self,
        stop_station_ids: Vec<RailStationId>,
        now: UtcSeconds,
    ) -> Result<ServiceId, AppError<S::Error>> {
        self.create_passenger_service_with_mode(
            stop_station_ids,
            ServiceDirectionMode::BothDirections,
            now,
        )
    }

    /// Creates and persists one Passenger Service using the requested direction mode.
    pub fn create_passenger_service_with_mode(
        &mut self,
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
        now: UtcSeconds,
    ) -> Result<ServiceId, AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            let service_id = create_service_with_mode(state, stop_station_ids, direction_mode)
                .map_err(AppError::Service)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(service_id)
            }
        })
    }

    /// Updates and persists the route of one unused Passenger Service.
    pub fn update_passenger_service(
        &mut self,
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        let direction_mode = self
            .state
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .map(|service| service.direction_mode)
            .ok_or(AppError::Service(ServiceError::ServiceNotFound {
                service_id,
            }))?;
        self.update_passenger_service_with_mode(service_id, stop_station_ids, direction_mode, now)
    }

    /// Updates and persists the route and direction mode of one unused Passenger Service.
    pub fn update_passenger_service_with_mode(
        &mut self,
        service_id: ServiceId,
        stop_station_ids: Vec<RailStationId>,
        direction_mode: ServiceDirectionMode,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            update_service_with_mode(state, service_id, stop_station_ids, direction_mode)
                .map_err(AppError::Service)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(())
            }
        })
    }

    /// Changes or clears the optional commercial name of one Passenger Service.
    pub fn update_passenger_service_name(
        &mut self,
        service_id: ServiceId,
        custom_name: Option<String>,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            rename_service(state, service_id, custom_name).map_err(AppError::Service)
        })
    }

    /// Deletes and persists an unused Passenger Service.
    pub fn delete_passenger_service(
        &mut self,
        service_id: ServiceId,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            delete_service(state, service_id).map_err(AppError::Service)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(())
            }
        })
    }

    /// Assigns one owned Train to a Passenger Service and persists the allocation.
    pub fn assign_train_to_passenger_service(
        &mut self,
        train_id: TrainId,
        service_id: ServiceId,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            assign_train_to_service(state, train_id, service_id)
                .map_err(AppError::ServiceAssignment)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(())
            }
        })
    }

    /// Clears one Train's Passenger Service allocation and persists the change.
    pub fn unassign_train_from_passenger_service(
        &mut self,
        train_id: TrainId,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            unassign_train_from_service(state, train_id).map_err(AppError::ServiceAssignment)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(())
            }
        })
    }

    /// Authorises a Manual Dispatch only if the advanced candidate can be
    /// saved. The departure timestamp is the command's effective timestamp.
    pub fn dispatch_journey(
        &mut self,
        train_id: TrainId,
        service_id: ServiceId,
        now: UtcSeconds,
    ) -> Result<crate::model::JourneyId, AppError<S::Error>> {
        self.transact(now, |state, effective_now| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            if state
                .player_company
                .fleet
                .assigned_service_id(train_id)
                .is_none()
            {
                return Err(AppError::Dispatch(
                    DispatchError::TrainNotAssignedToService {
                        train_id,
                        service_id,
                    },
                ));
            }
            let journey_id = dispatch_journey(state, train_id, service_id, effective_now)
                .map_err(AppError::Dispatch)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(journey_id)
            }
        })
    }

    /// Moves one assigned READY Train empty to a valid departure terminus of
    /// its Passenger Service and persists the resulting positioning Journey.
    pub fn position_train_for_service(
        &mut self,
        train_id: TrainId,
        service_id: ServiceId,
        destination_station_id: RailStationId,
        now: UtcSeconds,
    ) -> Result<crate::model::JourneyId, AppError<S::Error>> {
        self.transact(now, |state, effective_now| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            let journey_id = dispatch_positioning_journey(
                state,
                train_id,
                service_id,
                destination_station_id,
                effective_now,
            )
            .map_err(AppError::Dispatch)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(journey_id)
            }
        })
    }

    /// Compatibility helper for legacy destination-based callers.
    ///
    /// The player-facing Manual Dispatch flow now selects an existing
    /// Passenger Service and calls [`Self::dispatch_journey`] instead. This
    /// helper remains useful for recovery logic and older tests while that
    /// migration settles.
    pub fn dispatch_to_destination(
        &mut self,
        train_id: TrainId,
        destination_station_id: RailStationId,
        now: UtcSeconds,
    ) -> Result<crate::model::JourneyId, AppError<S::Error>> {
        self.transact(now, |state, effective_now| {
            let bankruptcy_prevents_operation = bankruptcy_prevents_operations(state)?;
            let origin_station_id = state
                .player_company
                .fleet
                .trains
                .iter()
                .find(|train| train.id == train_id)
                .ok_or(AppError::Dispatch(DispatchError::Quote(
                    EconomyError::TrainNotFound { train_id },
                )))
                .and_then(|train| match train.status {
                    TrainStatus::Ready { at } => Ok(at),
                    TrainStatus::Travelling { .. } => Err(AppError::Dispatch(
                        DispatchError::Quote(EconomyError::TrainTravelling { train_id }),
                    )),
                })?;
            let service_id =
                find_or_create_service(state, origin_station_id, destination_station_id)
                    .map_err(AppError::Service)?;
            assign_train_to_service(state, train_id, service_id)
                .map_err(AppError::ServiceAssignment)?;
            let journey_id = dispatch_journey(state, train_id, service_id, effective_now)
                .map_err(AppError::Dispatch)?;
            if bankruptcy_prevents_operation {
                Err(AppError::Bankruptcy)
            } else {
                Ok(journey_id)
            }
        })
    }

    /// Updates the Player Company's Vehicle Keeper Mark and persists it.
    pub fn update_company_vkm(
        &mut self,
        vehicle_keeper_mark: VehicleKeeperMark,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            state.player_company.vehicle_keeper_mark = vehicle_keeper_mark;
            Ok(())
        })
    }

    /// Changes or clears the player-facing nickname of one owned Train.
    pub fn update_train_nickname(
        &mut self,
        train_id: TrainId,
        nickname: Option<TrainNickname>,
        now: UtcSeconds,
    ) -> Result<(), AppError<S::Error>> {
        self.transact(now, |state, _| {
            crate::sim::fleet::rename_train(state, train_id, nickname).map_err(AppError::Rename)?;
            Ok(())
        })
    }

    /// Reconciles due Journeys and persists their settlement before publishing
    /// the resulting Train, receipts, and Company Funds.
    pub fn reconcile(&mut self, now: UtcSeconds) -> Result<(), AppError<S::Error>> {
        self.transact(now, |_, _| Ok(()))
    }

    fn transact<T>(
        &mut self,
        now: UtcSeconds,
        action: impl FnOnce(&mut GameState, UtcSeconds) -> Result<T, AppError<S::Error>>,
    ) -> Result<T, AppError<S::Error>> {
        let mut candidate = self.state.clone();
        advance_time(&mut candidate, now).map_err(AppError::Advance)?;
        let effective_now = candidate.last_processed_at;
        let result = action(&mut candidate, effective_now)?;
        self.store.save(&candidate).map_err(AppError::Save)?;
        self.state = candidate;
        Ok(result)
    }
}

impl App<SaveSlot> {
    /// Starts a fresh game only after Bankruptcy, preserving the former Player
    /// Company save in a unique sibling backup before publishing the restart.
    pub fn restart_after_bankruptcy(
        &mut self,
        world_seed: u64,
        now: UtcSeconds,
    ) -> Result<PathBuf, AppError<SaveSlotError>> {
        let evaluation = evaluate_financial_recovery(&self.state).map_err(AppError::Finance)?;
        if evaluation.status != FinancialStatus::Bankruptcy {
            return Err(AppError::RestartUnavailable {
                status: evaluation.status,
            });
        }
        let mut replacement =
            create_new_game(world_seed, self.state.player_company.name.clone(), now);
        replacement.player_company.vehicle_keeper_mark =
            self.state.player_company.vehicle_keeper_mark.clone();
        let backup_path = self
            .store
            .save_after_backup(&replacement)
            .map_err(AppError::Save)?;
        self.state = replacement;
        Ok(backup_path)
    }
}

fn bankruptcy_prevents_operations<E>(state: &GameState) -> Result<bool, AppError<E>> {
    let evaluation = evaluate_financial_recovery(state).map_err(AppError::Finance)?;
    Ok(evaluation.status == FinancialStatus::Bankruptcy)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        error::Error,
        fmt,
        rc::Rc,
    };

    use crate::{
        catalog::train_catalogue,
        model::{
            GameState, JourneyPurpose, RailStationId, ServiceDirectionMode, TrainNickname,
            TrainStatus, UtcSeconds,
        },
        sim::{
            economy::quote_journey,
            fleet::{FleetError, purchase_train},
            journeys::{DispatchError, dispatch_journey, dispatch_positioning_journey},
            services::{assign_train_to_service, find_or_create_service},
            world::create_new_game,
        },
    };

    use super::{App, AppCommand, AppCommandResult, AppError, GameStore};

    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);
    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(0);
    const DEPARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    #[derive(Clone, Debug, Default)]
    struct TestStore {
        saved: Rc<RefCell<Option<GameState>>>,
        fail_next_save: Rc<Cell<bool>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestStoreError {
        SimulatedWriteFailure,
    }

    impl fmt::Display for TestStoreError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "simulated write failure")
        }
    }

    impl Error for TestStoreError {}

    impl GameStore for TestStore {
        type Error = TestStoreError;

        fn load(&self) -> Result<Option<GameState>, Self::Error> {
            Ok(self.saved.borrow().clone())
        }

        fn save(&self, state: &GameState) -> Result<(), Self::Error> {
            if self.fail_next_save.replace(false) {
                return Err(TestStoreError::SimulatedWriteFailure);
            }
            self.saved.replace(Some(state.clone()));
            Ok(())
        }
    }

    fn new_game() -> GameState {
        create_new_game(42, "Alden Passenger", STARTED_AT)
    }

    fn dispatched_game() -> GameState {
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT).unwrap();
        state
    }

    #[test]
    fn fresh_player_company_is_saved_before_play_begins() {
        let store = TestStore::default();
        let state = new_game();

        let app = App::start_new(store.clone(), state.clone()).unwrap();

        assert_eq!(app.state(), &state);
        assert_eq!(store.load().unwrap(), Some(state));
    }

    #[test]
    fn execute_routes_train_nickname_through_the_application_boundary() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let nickname = TrainNickname::parse("North Star").unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();

        let result = app
            .execute(
                AppCommand::UpdateTrainNickname {
                    train_id,
                    nickname: Some(nickname.clone()),
                },
                STARTED_AT,
            )
            .unwrap();

        assert_eq!(result, AppCommandResult::TrainNicknameUpdated { train_id });
        assert_eq!(
            app.state().player_company.fleet.trains[0].nickname.as_ref(),
            Some(&nickname)
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn execute_rejects_an_invalid_command_without_changing_published_or_persisted_state() {
        let store = TestStore::default();
        let mut app = App::start_new(store.clone(), new_game()).unwrap();
        let before = app.state().clone();
        let missing_train_id = crate::model::TrainId::new(999);

        let error = app
            .execute(
                AppCommand::UpdateTrainNickname {
                    train_id: missing_train_id,
                    nickname: Some(TrainNickname::parse("North Star").unwrap()),
                },
                STARTED_AT,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            AppError::Rename(FleetError::TrainNotFound { train_id })
                if train_id == missing_train_id
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));
    }

    #[test]
    fn execute_does_not_publish_a_command_when_persistence_fails() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();
        let before = app.state().clone();
        let nickname = TrainNickname::parse("North Star").unwrap();
        store.fail_next_save.set(true);

        let error = app
            .execute(
                AppCommand::UpdateTrainNickname {
                    train_id,
                    nickname: Some(nickname.clone()),
                },
                STARTED_AT,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            AppError::Save(TestStoreError::SimulatedWriteFailure)
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));

        let result = app
            .execute(
                AppCommand::UpdateTrainNickname {
                    train_id,
                    nickname: Some(nickname.clone()),
                },
                STARTED_AT,
            )
            .unwrap();

        assert_eq!(result, AppCommandResult::TrainNicknameUpdated { train_id });
        assert_eq!(
            app.state().player_company.fleet.trains[0].nickname.as_ref(),
            Some(&nickname)
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn failed_purchase_is_not_published_and_can_be_retried() {
        let store = TestStore::default();
        let mut app = App::start_new(store.clone(), new_game()).unwrap();
        let before = app.state().clone();
        store.fail_next_save.set(true);

        assert!(matches!(
            app.execute(
                AppCommand::PurchaseTrain {
                    catalogue_index: 0,
                    delivery_station_id: ORIGIN,
                },
                STARTED_AT,
            ),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));

        let result = app
            .execute(
                AppCommand::PurchaseTrain {
                    catalogue_index: 0,
                    delivery_station_id: ORIGIN,
                },
                STARTED_AT,
            )
            .unwrap();
        let train_id = app.state().player_company.fleet.trains[0].id;
        assert_eq!(result, AppCommandResult::TrainPurchased { train_id });
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
        assert_eq!(app.state().player_company.fleet.trains.len(), 1);
    }

    #[test]
    fn failed_dispatch_is_not_published() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();
        let before = app.state().clone();
        store.fail_next_save.set(true);

        assert!(matches!(
            app.execute(
                AppCommand::ManualDispatch {
                    train_id,
                    service_id,
                },
                DEPARTED_AT,
            ),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));
    }

    #[test]
    fn execute_routes_passenger_service_lifecycle_through_the_application_boundary() {
        let store = TestStore::default();
        let mut app = App::start_new(store.clone(), new_game()).unwrap();
        let stops = vec![ORIGIN, DESTINATION];

        let created = app
            .execute(
                AppCommand::CreatePassengerService {
                    stop_station_ids: stops.clone(),
                    direction_mode: ServiceDirectionMode::BothDirections,
                },
                STARTED_AT,
            )
            .unwrap();
        let AppCommandResult::PassengerServiceCreated { service_id } = created else {
            panic!("expected PassengerServiceCreated command result");
        };

        assert_eq!(
            app.execute(
                AppCommand::UpdatePassengerService {
                    service_id,
                    stop_station_ids: stops,
                    direction_mode: ServiceDirectionMode::BothDirections,
                },
                STARTED_AT,
            )
            .unwrap(),
            AppCommandResult::PassengerServiceUpdated { service_id }
        );
        assert_eq!(
            app.execute(
                AppCommand::DeletePassengerService { service_id },
                STARTED_AT,
            )
            .unwrap(),
            AppCommandResult::PassengerServiceDeleted { service_id }
        );
        assert!(app.state().player_company.passenger_services.is_empty());
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn execute_persists_train_service_assignment_changes() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();

        assert_eq!(
            app.execute(
                AppCommand::AssignTrainToService {
                    train_id,
                    service_id,
                },
                STARTED_AT,
            )
            .unwrap(),
            AppCommandResult::TrainServiceAssigned {
                train_id,
                service_id,
            }
        );
        assert_eq!(
            app.state()
                .player_company
                .fleet
                .assigned_service_id(train_id),
            Some(service_id)
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());

        assert_eq!(
            app.execute(
                AppCommand::UnassignTrainFromService { train_id },
                STARTED_AT,
            )
            .unwrap(),
            AppCommandResult::TrainServiceUnassigned { train_id }
        );
        assert_eq!(
            app.state()
                .player_company
                .fleet
                .assigned_service_id(train_id),
            None
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn positioning_journey_persists_across_reload_without_revenue() {
        let store = TestStore::default();
        let mut state = new_game();
        let away_station = RailStationId::new(3);
        let train_id = purchase_train(&mut state, 0, away_station).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let funds_before = state.player_company.funds;
        let demand_before = state.origin_destination_demand.clone();
        let mut app = App::start_new(store.clone(), state).unwrap();

        let result = app
            .execute(
                AppCommand::PositionTrainForService {
                    train_id,
                    service_id,
                    destination_station_id: ORIGIN,
                },
                DEPARTED_AT,
            )
            .unwrap();
        let AppCommandResult::TrainPositioningStarted { journey_id } = result else {
            panic!("expected TrainPositioningStarted command result");
        };
        let journey = app
            .state()
            .active_journeys
            .iter()
            .find(|journey| journey.id == journey_id)
            .unwrap();
        assert_eq!(journey.purpose, JourneyPurpose::Positioning);
        assert_eq!(journey.passengers_carried, 0);
        assert_eq!(journey.operating_revenue, crate::model::Money::ZERO);
        assert!(app.state().player_company.funds < funds_before);
        assert_eq!(
            app.state().financials.operating_revenue,
            crate::model::Money::ZERO
        );
        assert!(app.state().financials.infrastructure_access_fees > crate::model::Money::ZERO);
        assert!(app.state().financials.fuel_costs > crate::model::Money::ZERO);
        assert_eq!(app.state().origin_destination_demand, demand_before);
        assert_eq!(
            app.state()
                .player_company
                .fleet
                .assigned_service_id(train_id),
            Some(service_id)
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());

        let persisted = app.state().clone();
        drop(app);
        let reloaded = App::load(store, DEPARTED_AT).unwrap().unwrap();
        assert_eq!(reloaded.state(), &persisted);
        assert_eq!(
            reloaded
                .state()
                .player_company
                .fleet
                .assigned_service_id(train_id),
            Some(service_id)
        );
    }

    #[test]
    fn execute_dispatches_a_journey_through_the_application_boundary() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();

        let result = app
            .execute(
                AppCommand::ManualDispatch {
                    train_id,
                    service_id,
                },
                DEPARTED_AT,
            )
            .unwrap();

        let journey_id = app.state().active_journeys[0].id;
        assert_eq!(result, AppCommandResult::JourneyDispatched { journey_id });
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn destination_dispatch_creates_a_service_only_with_a_persisted_journey() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let mut app = App::start_new(store.clone(), state).unwrap();
        let before = app.state().clone();
        store.fail_next_save.set(true);

        assert!(matches!(
            app.dispatch_to_destination(train_id, DESTINATION, DEPARTED_AT),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert!(app.state().player_company.passenger_services.is_empty());

        app.dispatch_to_destination(train_id, DESTINATION, DEPARTED_AT)
            .unwrap();
        assert_eq!(app.state().player_company.passenger_services.len(), 1);
        assert_eq!(app.state().active_journeys.len(), 1);
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn ready_train_resale_is_persisted_and_credits_its_proceeds() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let expected_proceeds = state.player_company.fleet.trains[0]
            .original_purchase_price
            .cents()
            * 70
            / 100;
        let funds_before_sale = state.player_company.funds;
        let mut app = App::start_new(store.clone(), state).unwrap();

        let result = app
            .execute(AppCommand::SellTrain { train_id }, STARTED_AT)
            .unwrap();
        let AppCommandResult::TrainSold {
            train_id: sold_train_id,
            proceeds,
        } = result
        else {
            panic!("expected TrainSold command result");
        };

        assert_eq!(sold_train_id, train_id);
        assert_eq!(proceeds.cents(), expected_proceeds);
        assert_eq!(
            app.state().player_company.funds.cents(),
            funds_before_sale.cents() + expected_proceeds
        );
        assert!(app.state().player_company.fleet.trains.is_empty());
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn destination_dispatch_revalidates_a_stale_quote_without_creating_a_service() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let mut preview = state.clone();
        let service_id = find_or_create_service(&mut preview, ORIGIN, DESTINATION).unwrap();
        let stale_quote = quote_journey(&preview, train_id, service_id).unwrap();
        state.player_company.funds =
            crate::model::Money::from_cents(stale_quote.operating_cost.cents() - 1);
        let mut app = App::start_new(store.clone(), state).unwrap();
        let before = app.state().clone();

        let error = app
            .dispatch_to_destination(train_id, DESTINATION, DEPARTED_AT)
            .unwrap_err();

        assert!(error.to_string().contains("Company Funds"));
        assert_eq!(app.state(), &before);
        assert!(app.state().player_company.passenger_services.is_empty());
        assert_eq!(store.load().unwrap(), Some(before));
    }

    #[test]
    fn destination_dispatch_uses_the_train_current_location_and_route() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let mut preview = state.clone();
        let stale_service_id = find_or_create_service(&mut preview, ORIGIN, DESTINATION).unwrap();
        let _stale_quote = quote_journey(&preview, train_id, stale_service_id).unwrap();
        state.player_company.fleet.trains[0].status = TrainStatus::Ready {
            at: RailStationId::new(3),
        };
        let mut app = App::start_new(store, state).unwrap();

        app.dispatch_to_destination(train_id, DESTINATION, DEPARTED_AT)
            .unwrap();

        assert_eq!(
            app.state().active_journeys[0].origin_station_id,
            RailStationId::new(3)
        );
        assert_eq!(
            app.state().player_company.passenger_services[0]
                .origin_station_id()
                .unwrap(),
            RailStationId::new(3)
        );
    }

    #[test]
    fn revenue_dispatch_rejects_an_unassigned_train_without_mutating_state() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        let mut app = App::start_new(store, state).unwrap();
        let before = app.state().clone();

        assert!(matches!(
            app.dispatch_journey(train_id, service_id, DEPARTED_AT),
            Err(AppError::Dispatch(
                DispatchError::TrainNotAssignedToService {
                    train_id: rejected_train_id,
                    service_id: rejected_service_id,
                }
            )) if rejected_train_id == train_id && rejected_service_id == service_id
        ));
        assert_eq!(app.state(), &before);
    }

    #[test]
    fn dispatch_uses_the_effective_time_after_a_backward_clock_read() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let mut app = App::start_new(store, state).unwrap();
        let later = UtcSeconds::from_unix_seconds(2_000);

        app.reconcile(later).unwrap();
        app.dispatch_journey(train_id, service_id, DEPARTED_AT)
            .unwrap();

        assert_eq!(app.state().active_journeys[0].departed_at, later);
    }

    #[test]
    fn failed_arrival_settlement_is_not_published() {
        let store = TestStore::default();
        let state = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        let mut app = App::start_new(store.clone(), state).unwrap();
        let before = app.state().clone();
        store.fail_next_save.set(true);

        assert!(matches!(
            app.reconcile(arrives_at),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));
    }

    #[test]
    fn reload_settles_an_elapsed_journey_once() {
        let store = TestStore::default();
        let state = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        App::start_new(store.clone(), state).unwrap();

        let first_load = App::load(store.clone(), arrives_at).unwrap().unwrap();
        let settled = first_load.state().clone();
        assert!(settled.active_journeys.is_empty());
        assert_eq!(
            settled.player_company.fleet.trains[0].status,
            TrainStatus::Ready { at: DESTINATION }
        );
        assert_eq!(settled.financials.recent_journey_receipts.len(), 1);
        drop(first_load);

        let second_load = App::load(store, arrives_at).unwrap().unwrap();
        assert_eq!(second_load.state(), &settled);
        assert_eq!(
            second_load.state().financials.recent_journey_receipts.len(),
            1
        );
    }

    #[test]
    fn loading_hands_off_only_the_arrivals_committed_by_that_load() {
        let store = TestStore::default();
        let state = dispatched_game();
        let journey = state.active_journeys[0].clone();
        App::start_new(store.clone(), state).unwrap();

        let loaded = App::load_or_empty(store.clone(), journey.arrives_at)
            .unwrap()
            .unwrap();
        let (first_load, arrivals) = loaded.into_parts();
        assert_eq!(arrivals.len(), 1);
        assert_eq!(arrivals[0].journey_id, journey.id);
        assert_eq!(arrivals[0].credited_revenue, journey.operating_revenue);
        drop(first_load);

        let loaded = App::load_or_empty(store, journey.arrives_at)
            .unwrap()
            .unwrap();
        let (_, arrivals) = loaded.into_parts();
        assert!(arrivals.is_empty());
    }

    #[test]
    fn failed_load_reconciliation_does_not_hand_off_arrivals() {
        let store = TestStore::default();
        let state = dispatched_game();
        let arrives_at = state.active_journeys[0].arrives_at;
        App::start_new(store.clone(), state.clone()).unwrap();
        store.fail_next_save.set(true);

        assert!(matches!(
            App::load_or_empty(store.clone(), arrives_at),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(store.load().unwrap(), Some(state));
    }

    #[test]
    fn missing_save_remains_an_onboarding_condition() {
        assert!(
            App::<TestStore>::load(TestStore::default(), STARTED_AT)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn passenger_service_creation_and_deletion_are_persisted_transactions() {
        let store = TestStore::default();
        let state = new_game();
        let mut app = App::start_new(store.clone(), state).unwrap();

        let service_id = app
            .create_passenger_service(
                vec![ORIGIN, RailStationId::new(2), RailStationId::new(3)],
                STARTED_AT,
            )
            .unwrap();

        let service = app
            .state()
            .player_company
            .passenger_services
            .iter()
            .find(|service| service.id == service_id)
            .unwrap();
        assert_eq!(service.name, "R1");
        assert_eq!(service.stop_station_ids.len(), 3);
        assert_eq!(store.load().unwrap(), Some(app.state().clone()));

        app.delete_passenger_service(service_id, STARTED_AT)
            .unwrap();

        assert!(app.state().player_company.passenger_services.is_empty());
        assert_eq!(store.load().unwrap(), Some(app.state().clone()));
    }

    #[test]
    fn execute_updates_company_vkm_through_the_application_boundary() {
        let store = TestStore::default();
        let mut app = App::start_new(store.clone(), new_game()).unwrap();
        let vehicle_keeper_mark = crate::model::VehicleKeeperMark::parse("OMP").unwrap();

        let result = app
            .execute(
                AppCommand::UpdateCompanyVkm {
                    vehicle_keeper_mark: vehicle_keeper_mark.clone(),
                },
                STARTED_AT,
            )
            .unwrap();

        assert_eq!(result, AppCommandResult::CompanyVkmUpdated);
        assert_eq!(
            app.state().player_company.vehicle_keeper_mark,
            vehicle_keeper_mark
        );
        assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    }

    #[test]
    fn bankruptcy_blocks_normal_operations_without_publishing_a_change() {
        let store = TestStore::default();
        let mut state = new_game();
        state.player_company.funds = train_catalogue().models()[0].purchase_price();
        let mut app = App::start_new(store.clone(), state.clone()).unwrap();

        assert!(matches!(
            app.purchase_train(0, ORIGIN, STARTED_AT),
            Err(AppError::Bankruptcy)
        ));
        assert_eq!(app.state(), &state);
        assert_eq!(store.load().unwrap(), Some(state));
    }
}
