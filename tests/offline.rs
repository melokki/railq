//! Regression scenarios for closing and reopening a persisted Player Company.
//!
//! Each load uses a new [`SaveSlot`] to exercise the same close/reopen path as
//! an offline player, rather than only advancing an in-memory [`GameState`].

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use railq::{
    app::App,
    model::{
        DurationSeconds, GameState, MarketMaturity, Money, PassengerArrivalRate, RailStationId,
        TrainStatus, UtcSeconds,
    },
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    storage::SaveSlot,
};

const ORIGIN: RailStationId = RailStationId::new(1);
const DESTINATION: RailStationId = RailStationId::new(2);
const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(0);
const DEPARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

static NEXT_TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "railq-offline-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("test directory is created");
        Self { path }
    }

    fn save_path(&self) -> PathBuf {
        self.path.join("company.db")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn travelling_game() -> GameState {
    let mut state = create_new_game(42, "Offline Passenger", STARTED_AT);
    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("Train purchase succeeds");
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("Service is created");
    dispatch_journey(&mut state, train_id, service_id, DEPARTED_AT)
        .expect("Manual Dispatch succeeds");
    state
}

fn persist(path: &Path, state: GameState) {
    let slot = SaveSlot::open(path).expect("save slot opens");
    App::start_new(slot, state).expect("initial state persists");
}

fn reopen(path: &Path, now: UtcSeconds) -> GameState {
    let slot = SaveSlot::open(path).expect("save slot reopens");
    App::load(slot, now)
        .expect("saved Player Company loads")
        .expect("save exists")
        .state()
        .clone()
}

#[test]
fn load_before_eta_leaves_the_train_travelling() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    persist(&path, state);

    let loaded = reopen(
        &path,
        UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() - 1),
    );

    assert_eq!(loaded.active_journeys, vec![journey.clone()]);
    assert_eq!(
        loaded.player_company.fleet.trains[0].status,
        TrainStatus::Travelling {
            journey_id: journey.id
        }
    );
    assert_eq!(loaded.financials.operating_revenue, Money::ZERO);
}

#[test]
fn load_at_eta_settles_the_journey_once() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    persist(&path, state);

    let settled = reopen(&path, journey.arrives_at);
    assert_settled_at_destination(&settled, &journey);

    let reloaded = reopen(&path, journey.arrives_at);
    assert_eq!(reloaded, settled);
}

#[test]
fn load_long_after_eta_settles_once_and_leaves_the_train_waiting() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    persist(&path, state);

    let loaded = reopen(
        &path,
        UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() + 86_400),
    );

    assert_settled_at_destination(&loaded, &journey);
}

#[test]
fn repeated_save_reload_cycles_do_not_repeat_journey_accounting() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    let funds_after_departure = state.player_company.funds;
    let access_fees_after_departure = state.financials.infrastructure_access_fees;
    let fuel_costs_after_departure = state.financials.fuel_costs;
    persist(&path, state);

    for offset in [0, 1, 3_600, 86_400] {
        let loaded = reopen(
            &path,
            UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() + offset),
        );

        assert_eq!(
            loaded.player_company.funds,
            funds_after_departure
                .checked_add(journey.operating_revenue)
                .expect("fixture total is representable")
        );
        assert_eq!(
            loaded.financials.operating_revenue,
            journey.operating_revenue
        );
        assert_eq!(
            loaded.financials.infrastructure_access_fees,
            access_fees_after_departure
        );
        assert_eq!(loaded.financials.fuel_costs, fuel_costs_after_departure);
        assert_eq!(loaded.financials.recent_journey_receipts.len(), 1);
        assert_eq!(
            loaded.financials.recent_journey_receipts[0].journey_id,
            journey.id
        );
    }
}

#[test]
fn a_backward_clock_cannot_reverse_progress_or_duplicate_effects() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    persist(&path, state);

    let settled = reopen(&path, journey.arrives_at);
    let backward_clock = reopen(
        &path,
        UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() - 1),
    );

    assert_eq!(backward_clock, settled);
}

#[test]
fn offline_advancement_keeps_waiting_passengers_at_the_configured_cap() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let mut state = create_new_game(42, "Offline Passenger", STARTED_AT);
    state.rules.demand.cap_duration = DurationSeconds::from_seconds(3_600);
    for pool in &mut state.origin_destination_demand {
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour = PassengerArrivalRate::new(3).unwrap();
        pool.market_maturity = MarketMaturity::full();
        pool.fractional_passenger_seconds = 0;
    }
    persist(&path, state);

    let capped = reopen(&path, UtcSeconds::from_unix_seconds(7 * 24 * 60 * 60));
    assert!(
        capped
            .origin_destination_demand
            .iter()
            .all(|pool| { pool.waiting_passengers == 3 && pool.fractional_passenger_seconds == 0 })
    );

    let still_capped = reopen(&path, UtcSeconds::from_unix_seconds(14 * 24 * 60 * 60));
    assert_eq!(
        still_capped.origin_destination_demand,
        capped.origin_destination_demand
    );
}

#[test]
fn reload_never_creates_an_automatic_departure() {
    let directory = TestDirectory::new();
    let path = directory.save_path();
    let state = travelling_game();
    let journey = state.active_journeys[0].clone();
    persist(&path, state);

    let after_arrival = reopen(
        &path,
        UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() + 86_400),
    );
    let later_reload = reopen(
        &path,
        UtcSeconds::from_unix_seconds(journey.arrives_at.unix_seconds() + 172_800),
    );

    assert_settled_at_destination(&after_arrival, &journey);
    assert_settled_at_destination(&later_reload, &journey);
}

fn assert_settled_at_destination(state: &GameState, journey: &railq::model::Journey) {
    assert!(state.active_journeys.is_empty());
    assert_eq!(
        state.player_company.fleet.trains[0].status,
        TrainStatus::Ready { at: DESTINATION }
    );
    assert_eq!(
        state.financials.operating_revenue,
        journey.operating_revenue
    );
    assert_eq!(state.financials.recent_journey_receipts.len(), 1);
    assert_eq!(
        state.financials.recent_journey_receipts[0].journey_id,
        journey.id
    );
}
