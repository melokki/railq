//! Keyboard, transaction, and rendered-buffer coverage for Fleet dispatch.

use std::{cell::RefCell, convert::Infallible, error::Error, fs, path::Path, rc::Rc};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    app::{App, GameStore},
    model::{JourneyId, Money, RailStationId, TrainStatus, UtcSeconds},
    sim::{fleet::purchase_train, services::create_service, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/16";

#[derive(Clone, Debug, Default)]
struct TestStore {
    saved: Rc<RefCell<Option<railq::model::GameState>>>,
}

impl GameStore for TestStore {
    type Error = Infallible;

    fn load(&self) -> Result<Option<railq::model::GameState>, Self::Error> {
        Ok(self.saved.borrow().clone())
    }

    fn save(&self, state: &railq::model::GameState) -> Result<(), Self::Error> {
        self.saved.replace(Some(state.clone()));
        Ok(())
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ready_fleet() -> railq::model::GameState {
    let mut state = create_new_game(42, "Fleet Dispatch Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("first Train is purchased");
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("second Train is purchased");
    create_service(
        &mut state,
        vec![RailStationId::new(1), RailStationId::new(3)],
    )
    .expect("dispatch Service is created");
    state
}

#[test]
fn fleet_d_preselects_the_selected_train_and_returns_to_its_list_focus()
-> Result<(), Box<dyn Error>> {
    let state = ready_fleet();
    let mut shell = Shell::new();
    fs::create_dir_all(EVIDENCE_DIR)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('t')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Down), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('d')), &state),
        ShellAction::Continue
    );

    for (columns, rows, file_name) in [
        (120, 40, "fleet-dispatch-service-120x40.txt"),
        (80, 24, "fleet-dispatch-service-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("2 SERVICE"));
        assert!(rendered.contains("Train 02"));
        assert!(rendered.contains("Choose Passenger Service"));
        assert!(!rendered.contains("available Fleet"));
        if columns == 120 {
            let (row, column) = rendered
                .lines()
                .enumerate()
                .find_map(|(row, line)| line.find("R1").map(|column| (row, column)))
                .expect("the available Service remains visible");
            assert_eq!(
                capture_rendered_cell_colors(
                    &shell,
                    &state,
                    columns,
                    rows,
                    u16::try_from(column)?,
                    u16::try_from(row)?,
                ),
                Some((theme::BACKGROUND, theme::ACCENT))
            );
        }
        fs::write(Path::new(EVIDENCE_DIR).join(file_name), rendered)?;
    }

    assert_eq!(
        shell.handle_key(key(KeyCode::Left), &state),
        ShellAction::Continue
    );
    let train_step = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(train_step.contains("Manual Dispatch · available Fleet"));
    assert!(train_step.contains("› Train 02"));

    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    let returned = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(returned.contains("› Train 02"));
    assert!(returned.contains("[D] Dispatch"));
    assert!(!returned.contains("Train details"));

    Ok(())
}

#[test]
fn fleet_d_explains_why_a_travelling_train_is_unavailable_and_refreshes_eligibility() {
    let mut state = ready_fleet();
    let selected_train_id = state.player_company.fleet.trains[1].id;
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('t')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Down), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('d')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );

    state.player_company.fleet.trains[1].status = TrainStatus::Travelling {
        journey_id: JourneyId::new(999),
    };
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    let refreshed = capture_rendered_buffer(&shell, &state, 120, 40);
    assert!(refreshed.contains("Journey quote could not be revalidated"));
    assert!(refreshed.contains("no longer READY"));

    let mut unavailable_shell = Shell::new();
    assert_eq!(
        unavailable_shell.handle_key(key(KeyCode::Char('t')), &state),
        ShellAction::Continue
    );
    assert_eq!(
        unavailable_shell.handle_key(key(KeyCode::Down), &state),
        ShellAction::Continue
    );
    assert_eq!(
        unavailable_shell.handle_key(key(KeyCode::Char('d')), &state),
        ShellAction::Continue
    );
    let unavailable = capture_rendered_buffer(&unavailable_shell, &state, 120, 40);
    assert!(unavailable.contains(&format!(
        "Train {} is TRAVELLING and cannot be dispatched",
        selected_train_id.get()
    )));
    assert!(unavailable.contains("TRAVELLING"));
}

#[test]
fn fleet_dispatch_commits_through_the_existing_saved_manual_dispatch_transaction() {
    let store = TestStore::default();
    let mut app = App::start_new(store.clone(), ready_fleet()).expect("state is saved initially");
    let selected_train_id = app.state().player_company.fleet.trains[1].id;
    let funds_before = app.state().player_company.funds;
    let mut shell = Shell::new();

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('t')), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Down), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('d')), app.state()),
        ShellAction::Continue
    );
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), app.state()),
        ShellAction::Continue
    );
    let action = shell.handle_key(key(KeyCode::Enter), app.state());
    let ShellAction::ManualDispatch { train_id, service_id } = action
    else {
        panic!("Fleet review should request the existing Manual Dispatch transaction");
    };
    assert_eq!(train_id, selected_train_id);

    app.dispatch_journey(train_id, service_id, STARTED_AT)
        .expect("the normal saved transaction authorises the Journey");
    shell.confirm_manual_dispatch();

    assert!(app.state().player_company.funds < funds_before);
    assert!(matches!(
        app.state()
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == selected_train_id)
            .expect("selected Train remains in the Fleet")
            .status,
        TrainStatus::Travelling { .. }
    ));
    assert_eq!(
        store.load().expect("saved state is readable"),
        Some(app.state().clone())
    );

    let saved = capture_rendered_buffer(&shell, app.state(), 120, 40);
    assert!(saved.contains("Manual Dispatch authorised and saved."));
    assert!(saved.contains("TRAVELLING"));
}
