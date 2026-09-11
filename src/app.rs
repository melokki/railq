//! Application-boundary orchestration for persisted RailQ commands.
//!
//! Simulation transitions operate on a candidate [`GameState`]. The candidate
//! is written before it replaces the state visible to the rest of the app, so
//! a failed save never publishes a partial Player Company action.

use std::{error::Error, fmt};

use crate::{
    model::{GameState, RailStationId, ServiceId, TrainId, TrainStatus, UtcSeconds},
    sim::{
        economy::EconomyError,
        fleet::{FleetError, purchase_train},
        journeys::{DispatchError, dispatch_journey},
        services::{ServiceError, find_or_create_service},
        time::{AdvanceTimeError, advance_time},
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
    /// A Manual Dispatch was rejected by the simulation.
    Dispatch(DispatchError),
    /// A Passenger Service could not be created or reused for a Manual Dispatch.
    Service(ServiceError),
}

impl<E: fmt::Display> fmt::Display for AppError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(formatter, "could not load saved game: {error}"),
            Self::Save(error) => write!(formatter, "could not save game changes: {error}"),
            Self::Advance(error) => error.fmt(formatter),
            Self::Purchase(error) => error.fmt(formatter),
            Self::Dispatch(error) => error.fmt(formatter),
            Self::Service(error) => error.fmt(formatter),
        }
    }
}

impl<E: Error + 'static> Error for AppError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Load(error) | Self::Save(error) => Some(error),
            Self::Advance(error) => Some(error),
            Self::Service(error) => Some(error),
            Self::Purchase(error) => Some(error),
            Self::Dispatch(error) => Some(error),
        }
    }
}

/// The published game state and its exclusively owned persistence boundary.
#[derive(Debug)]
pub struct App<S> {
    state: GameState,
    store: S,
}

impl<S: GameStore> App<S> {
    /// Persists a freshly created Player Company before allowing play.
    pub fn start_new(store: S, state: GameState) -> Result<Self, AppError<S::Error>> {
        store.save(&state).map_err(AppError::Save)?;
        Ok(Self { state, store })
    }

    /// Loads a save, reconciles elapsed Journeys, and persists that result
    /// before exposing it to the caller. A missing save remains an onboarding
    /// condition rather than a silently generated fresh game.
    pub fn load(store: S, now: UtcSeconds) -> Result<Option<Self>, AppError<S::Error>> {
        Ok(Self::load_or_empty(store, now)?.ok())
    }

    /// Loads a saved Player Company or returns the still-owned empty store for
    /// onboarding. This keeps the exclusive save-slot lock open between a
    /// missing-save check and the first persisted Player Company.
    pub fn load_or_empty(store: S, now: UtcSeconds) -> Result<Result<Self, S>, AppError<S::Error>> {
        let Some(mut state) = store.load().map_err(AppError::Load)? else {
            return Ok(Err(store));
        };
        advance_time(&mut state, now).map_err(AppError::Advance)?;
        store.save(&state).map_err(AppError::Save)?;
        Ok(Ok(Self { state, store }))
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
            purchase_train(state, catalogue_index, delivery_station_id).map_err(AppError::Purchase)
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
            dispatch_journey(state, train_id, service_id, effective_now).map_err(AppError::Dispatch)
        })
    }

    /// Creates or reuses the selected Passenger Service and authorises its
    /// Manual Dispatch as one persisted transaction.
    ///
    /// The origin is deliberately read from the current READY Train inside
    /// the candidate transaction. A destination selected while reviewing a
    /// quote is therefore never trusted as a stale route or Train location.
    pub fn dispatch_to_destination(
        &mut self,
        train_id: TrainId,
        destination_station_id: RailStationId,
        now: UtcSeconds,
    ) -> Result<crate::model::JourneyId, AppError<S::Error>> {
        self.transact(now, |state, effective_now| {
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
            dispatch_journey(state, train_id, service_id, effective_now).map_err(AppError::Dispatch)
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

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        error::Error,
        fmt,
        rc::Rc,
    };

    use crate::{
        model::{GameState, RailStationId, TrainStatus, UtcSeconds},
        sim::{
            economy::quote_journey, fleet::purchase_train, journeys::dispatch_journey,
            services::find_or_create_service, world::create_new_game,
        },
    };

    use super::{App, AppError, GameStore};

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
    fn failed_purchase_is_not_published_and_can_be_retried() {
        let store = TestStore::default();
        let mut app = App::start_new(store.clone(), new_game()).unwrap();
        let before = app.state().clone();
        store.fail_next_save.set(true);

        assert!(matches!(
            app.purchase_train(0, ORIGIN, STARTED_AT),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));

        app.purchase_train(0, ORIGIN, STARTED_AT).unwrap();
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
            app.dispatch_journey(train_id, service_id, DEPARTED_AT),
            Err(AppError::Save(TestStoreError::SimulatedWriteFailure))
        ));
        assert_eq!(app.state(), &before);
        assert_eq!(store.load().unwrap(), Some(before));
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
            app.state().player_company.passenger_services[0].first_station_id,
            RailStationId::new(3)
        );
    }

    #[test]
    fn dispatch_uses_the_effective_time_after_a_backward_clock_read() {
        let store = TestStore::default();
        let mut state = new_game();
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
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
    fn missing_save_remains_an_onboarding_condition() {
        assert!(
            App::<TestStore>::load(TestStore::default(), STARTED_AT)
                .unwrap()
                .is_none()
        );
    }
}
