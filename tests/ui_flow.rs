//! Keyboard-to-state regressions for the playable terminal flow.
//!
//! These tests drive [`railq::ui::Shell`] with the same keys a player uses, then
//! commit only the returned application-boundary action. They verify that
//! cancelled or rejected proposals remain presentation-only.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    app::{App, GameStore},
    model::{GameState, Money, RailStationId, TrainStatus, UtcSeconds},
    sim::time::advance_time,
    ui::{
        Shell, ShellAction, View, capture_rendered_buffer,
        start::{CompanyName, Startup, onboarding_summary, start},
    },
};
use std::{
    cell::{Cell, RefCell},
    convert::Infallible,
    error::Error,
    fmt, fs,
    path::Path,
    rc::Rc,
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);
const OUTBOUND_DEPARTURE: UtcSeconds = UtcSeconds::from_unix_seconds(2_000);
const OUTCOME_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/23";

#[derive(Clone, Debug, Default)]
struct TestStore {
    saved: Rc<RefCell<Option<GameState>>>,
}

#[derive(Clone, Debug, Default)]
struct RejectingStore {
    saved: Rc<RefCell<Option<GameState>>>,
    reject_next_save: Rc<Cell<bool>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RejectedSave;

impl fmt::Display for RejectedSave {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("simulated save rejection")
    }
}

impl Error for RejectedSave {}

impl GameStore for RejectingStore {
    type Error = RejectedSave;

    fn load(&self) -> Result<Option<GameState>, Self::Error> {
        Ok(self.saved.borrow().clone())
    }

    fn save(&self, state: &GameState) -> Result<(), Self::Error> {
        if self.reject_next_save.replace(false) {
            return Err(RejectedSave);
        }
        self.saved.replace(Some(state.clone()));
        Ok(())
    }
}

impl GameStore for TestStore {
    type Error = Infallible;

    fn load(&self) -> Result<Option<GameState>, Self::Error> {
        Ok(self.saved.borrow().clone())
    }

    fn save(&self, state: &GameState) -> Result<(), Self::Error> {
        self.saved.replace(Some(state.clone()));
        Ok(())
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn handle_action(
    shell: &mut Shell,
    app: &mut App<TestStore>,
    action: ShellAction,
    now: UtcSeconds,
) {
    match action {
        ShellAction::ManualDispatch {
            train_id,
            destination_station_id,
        } => {
            app.dispatch_to_destination(train_id, destination_station_id, now)
                .unwrap();
            shell.confirm_manual_dispatch();
        }
        ShellAction::PurchaseTrain {
            catalogue_index,
            delivery_station_id,
        } => {
            app.purchase_train(catalogue_index, delivery_station_id, now)
                .unwrap();
            shell.confirm_purchase_train();
        }
        ShellAction::SellTrain { train_id } => {
            let proceeds = app.sell_train(train_id, now).unwrap();
            shell.confirm_train_resale(proceeds);
        }
        ShellAction::CreatePassengerService { stop_station_ids } => {
            app.create_passenger_service(stop_station_ids, now).unwrap();
            shell.confirm_passenger_service_created(app.state());
        }
        ShellAction::DeletePassengerService { service_id } => {
            app.delete_passenger_service(service_id, now).unwrap();
            shell.confirm_passenger_service_deleted(app.state());
        }
        ShellAction::Continue | ShellAction::Exit | ShellAction::RestartAfterBankruptcy => {}
    }
}

fn press(shell: &mut Shell, app: &mut App<TestStore>, code: KeyCode, now: UtcSeconds) {
    let action = shell.handle_key(key(code), app.state());
    handle_action(shell, app, action, now);
}

fn buy_first_catalogue_train(shell: &mut Shell, app: &mut App<TestStore>, now: UtcSeconds) {
    press(shell, app, KeyCode::Enter, now);
    press(shell, app, KeyCode::Enter, now);
    press(shell, app, KeyCode::Enter, now);
}

fn dispatch_first_ready_train(shell: &mut Shell, app: &mut App<TestStore>, now: UtcSeconds) {
    press(shell, app, KeyCode::Char('d'), now);
    press(shell, app, KeyCode::Enter, now);
    press(shell, app, KeyCode::Enter, now);
    press(shell, app, KeyCode::Enter, now);
}

fn sell_first_ready_train(shell: &mut Shell, app: &mut App<TestStore>, now: UtcSeconds) {
    press(shell, app, KeyCode::Char('s'), now);
    press(shell, app, KeyCode::Enter, now);
}

fn dashboard_from_fresh_launch() -> (TestStore, App<TestStore>) {
    let store = TestStore::default();
    let Startup::Onboarding(onboarding) = start(store.clone(), STARTED_AT).unwrap() else {
        panic!("fresh launch should start onboarding");
    };
    let state = onboarding.prepare_company(
        CompanyName::parse("UI Flow Passenger").unwrap(),
        42,
        STARTED_AT,
    );
    let summary = onboarding_summary(&state);
    assert!(summary.contains("Region:"));
    assert!(summary.contains("Concession:"));
    assert!(summary.contains("Company Funds:"));

    let app = onboarding.save(state.clone()).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
    (store, app)
}

#[test]
fn fresh_launch_buy_dispatch_arrive_return_and_resale_are_keyboard_reachable() {
    let (store, mut app) = dashboard_from_fresh_launch();
    let mut shell = Shell::new();

    press(&mut shell, &mut app, KeyCode::Char('b'), STARTED_AT);
    assert_eq!(shell.active_view(), View::BuyTrains);
    buy_first_catalogue_train(&mut shell, &mut app, STARTED_AT);
    assert_eq!(app.state().player_company.fleet.trains.len(), 1);
    let train_id = app.state().player_company.fleet.trains[0].id;
    assert_eq!(
        app.state().player_company.fleet.trains[0].status,
        TrainStatus::Ready {
            at: RailStationId::new(1)
        }
    );

    press(&mut shell, &mut app, KeyCode::Char('m'), OUTBOUND_DEPARTURE);
    dispatch_first_ready_train(&mut shell, &mut app, OUTBOUND_DEPARTURE);
    assert_eq!(app.state().active_journeys.len(), 1);
    let outbound = app.state().active_journeys[0].clone();
    assert_eq!(outbound.origin_station_id, RailStationId::new(1));
    assert_eq!(outbound.destination_station_id, RailStationId::new(2));

    app.reconcile(outbound.arrives_at).unwrap();
    assert!(app.state().active_journeys.is_empty());
    assert_eq!(
        app.state().player_company.fleet.trains[0].status,
        TrainStatus::Ready {
            at: RailStationId::new(2)
        }
    );
    assert_eq!(app.state().financials.recent_journey_receipts.len(), 1);

    let return_departure =
        UtcSeconds::from_unix_seconds(outbound.arrives_at.unix_seconds().saturating_add(1));
    dispatch_first_ready_train(&mut shell, &mut app, return_departure);
    assert_eq!(app.state().active_journeys.len(), 1);
    let return_trip = app.state().active_journeys[0].clone();
    assert_eq!(return_trip.origin_station_id, RailStationId::new(2));
    assert_eq!(return_trip.destination_station_id, RailStationId::new(1));

    app.reconcile(return_trip.arrives_at).unwrap();
    press(
        &mut shell,
        &mut app,
        KeyCode::Char('t'),
        return_trip.arrives_at,
    );
    assert_eq!(shell.active_view(), View::Trains);
    sell_first_ready_train(&mut shell, &mut app, return_trip.arrives_at);

    assert!(app.state().player_company.fleet.trains.is_empty());
    assert_eq!(app.state().financials.recent_journey_receipts.len(), 2);
    assert_eq!(app.state(), store.load().unwrap().as_ref().unwrap());
    assert_eq!(train_id.get(), 1);
}

#[test]
fn cancellation_and_rejected_error_paths_preserve_player_company_state() {
    let (_, mut app) = dashboard_from_fresh_launch();
    let mut shell = Shell::new();

    press(&mut shell, &mut app, KeyCode::Char('b'), STARTED_AT);
    press(&mut shell, &mut app, KeyCode::Enter, STARTED_AT);
    let before_cancelled_purchase = app.state().clone();
    press(&mut shell, &mut app, KeyCode::Esc, STARTED_AT);
    assert_eq!(app.state(), &before_cancelled_purchase);
    assert!(app.state().player_company.fleet.trains.is_empty());

    buy_first_catalogue_train(&mut shell, &mut app, STARTED_AT);
    let before_cancelled_dispatch = app.state().clone();
    press(&mut shell, &mut app, KeyCode::Char('m'), OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Char('d'), OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Enter, OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Esc, OUTBOUND_DEPARTURE);
    assert_eq!(app.state(), &before_cancelled_dispatch);
    assert!(app.state().active_journeys.is_empty());
    assert!(app.state().player_company.passenger_services.is_empty());

    press(&mut shell, &mut app, KeyCode::Char('d'), OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Enter, OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Enter, OUTBOUND_DEPARTURE);
    let before_rejected_dispatch = app.state().clone();
    let action = shell.handle_key(key(KeyCode::Enter), app.state());
    let ShellAction::ManualDispatch {
        train_id,
        destination_station_id,
    } = action
    else {
        panic!("dispatch confirmation should request an application command");
    };
    let mut stale_state = app.state().clone();
    stale_state.player_company.funds = Money::ZERO;
    let mut stale_app = App::start_new(TestStore::default(), stale_state).unwrap();
    let error = stale_app
        .dispatch_to_destination(train_id, destination_station_id, OUTBOUND_DEPARTURE)
        .unwrap_err();
    shell.reject_manual_dispatch(error.to_string());
    assert_eq!(app.state(), &before_rejected_dispatch);

    press(&mut shell, &mut app, KeyCode::Esc, OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Char('t'), OUTBOUND_DEPARTURE);
    press(&mut shell, &mut app, KeyCode::Enter, OUTBOUND_DEPARTURE);
    let before_cancelled_resale = app.state().clone();
    press(&mut shell, &mut app, KeyCode::Esc, OUTBOUND_DEPARTURE);
    assert_eq!(app.state(), &before_cancelled_resale);
    assert_eq!(app.state().player_company.fleet.trains.len(), 1);
}

#[test]
fn failed_resale_save_keeps_the_review_open_without_a_success_notice() {
    let store = RejectingStore::default();
    let mut state = railq::sim::world::create_new_game(42, "Save Failure Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    let mut app = App::start_new(store.clone(), state).unwrap();
    let train_id = app
        .purchase_train(0, RailStationId::new(1), STARTED_AT)
        .unwrap();
    let before = app.state().clone();
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('t')), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('s')), app.state()),
        ShellAction::Continue
    );
    store.reject_next_save.set(true);
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), app.state()),
        ShellAction::SellTrain { train_id }
    );
    let error = app.sell_train(train_id, STARTED_AT).unwrap_err();
    shell.reject_train_resale(error.to_string());

    assert_eq!(app.state(), &before);
    assert_eq!(store.load().unwrap(), Some(before));
    let rendered = railq::ui::capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(
        rendered.contains("Resale rejected: could not save game changes: simulated save rejection")
    );
    assert!(!rendered.contains("Train resold and saved"));
}

#[test]
fn saved_purchase_has_an_inspectable_outcome_until_acknowledged() -> Result<(), Box<dyn Error>> {
    let (_, mut app) = dashboard_from_fresh_launch();
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('b')), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), app.state()),
        ShellAction::Continue
    );
    let ShellAction::PurchaseTrain {
        catalogue_index,
        delivery_station_id,
    } = shell.handle_key(key(KeyCode::Enter), app.state())
    else {
        panic!("purchase review should request an application command");
    };
    app.purchase_train(catalogue_index, delivery_station_id, STARTED_AT)
        .unwrap();
    shell.confirm_purchase_train_saved(app.state());

    let banner = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(banner.contains("Train purchase ·"));
    assert!(banner.contains("saved — Company Funds -$"));
    assert!(banner.contains("[Enter] Read"));

    let evidence_dir = Path::new(OUTCOME_EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    fs::write(evidence_dir.join("purchase-saved-120x40.txt"), &banner)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), app.state()),
        ShellAction::Continue
    );
    let details = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(details.contains("Saved action outcome"));
    assert!(details.contains("Company Funds:"));
    assert!(details.contains("Saved successfully."));
    fs::write(
        evidence_dir.join("purchase-outcome-details-120x40.txt"),
        &details,
    )?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('a')), app.state()),
        ShellAction::Continue
    );
    let dismissed = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(!dismissed.contains("Saved action outcome"));
    Ok(())
}

#[test]
fn empty_fleet_renderers_and_dispatch_entry_are_safe() {
    let (_, app) = dashboard_from_fresh_launch();
    let state = app.state();

    let map = railq::ui::map::render_at(state, STARTED_AT);
    let fleet = railq::ui::fleet::render_at(state, STARTED_AT);
    assert!(map.contains("No Trains in the Fleet"));
    assert!(fleet.contains("No Trains in the Fleet"));

    let mut shell = Shell::new();
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('d')), state),
        ShellAction::Continue
    );
    assert_eq!(state, app.state());
}

#[test]
fn renderer_strings_survive_operating_state_edges() {
    let (_, mut app) = dashboard_from_fresh_launch();
    app.purchase_train(0, RailStationId::new(1), STARTED_AT)
        .unwrap();
    let train_id = app.state().player_company.fleet.trains[0].id;
    app.dispatch_to_destination(train_id, RailStationId::new(2), OUTBOUND_DEPARTURE)
        .unwrap();
    let mut travelling = app.state().clone();
    let arrives_at = travelling.active_journeys[0].arrives_at;

    let travelling_map = railq::ui::map::render_at(&travelling, OUTBOUND_DEPARTURE);
    let travelling_fleet = railq::ui::fleet::render_at(&travelling, OUTBOUND_DEPARTURE);
    assert!(travelling_map.contains("TRAVELLING"));
    assert!(travelling_map.contains("ETA:"));
    assert!(travelling_fleet.contains("Sale unavailable"));

    advance_time(&mut travelling, arrives_at).unwrap();
    let company = railq::ui::company::render(&travelling);
    assert!(company.contains("Company Funds:"));
    assert!(company.contains("Latest receipts:"));
}
