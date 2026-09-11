//! Rendered-buffer and keyboard coverage for the Manual Dispatch Train chooser.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, TrainStatus, UtcSeconds},
    sim::{fleet::purchase_train, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/13";
const DESTINATION_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/14";
const QUOTE_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/15";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(key(code), state)
}

fn state_with_ready_trains() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).expect("first Train is purchased");
    purchase_train(&mut state, 0, RailStationId::new(2)).expect("second Train is purchased");
    state
}

#[test]
fn map_dispatch_prefers_the_focused_station_and_renders_a_stateful_ready_train_chooser()
-> Result<(), Box<dyn Error>> {
    let state = state_with_ready_trains();
    let before = state.clone();
    let mut shell = Shell::new();
    fs::create_dir_all(EVIDENCE_DIR)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("1 Train → 2 Destination → 3 Review"));
    assert!(wide.contains("Manual Dispatch · available Fleet"));
    assert!(wide.contains("Train 01"));
    assert!(wide.contains("Train 02"));
    assert!(wide.contains("READY"));
    assert!(wide.contains("Location"));
    assert!(wide.contains("a READY Train here is preselected"));
    let (row, column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Train 01").map(|column| (row, column)))
        .expect("focused Rail Station's Train is visibly selected");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column).expect("test terminal fits u16"),
            u16::try_from(row).expect("test terminal fits u16"),
        ),
        Some((theme::BACKGROUND, theme::ACCENT)),
    );
    fs::write(
        Path::new(EVIDENCE_DIR).join("train-chooser-120x40.txt"),
        &wide,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(!compact.contains("No READY Train at"));
    assert!(compact.contains("> Train 02"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("train-chooser-80x24.txt"),
        compact,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let destination = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(destination.contains("reachable destinations"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let fallback = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(fallback.contains("No READY Train at"));
    assert!(fallback.contains("Train 01"));
    assert!(fallback.contains("At Pinewatch"));
    assert!(fallback.contains("Train 02"));
    assert!(fallback.contains("At Oakridge"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        state, before,
        "Train selection and cancellation are presentation-only"
    );
    Ok(())
}

#[test]
fn destination_chooser_shows_quote_route_context_and_keeps_the_draft_recoverable()
-> Result<(), Box<dyn Error>> {
    let state = state_with_ready_trains();
    let before = state.clone();
    let mut shell = Shell::new();
    fs::create_dir_all(DESTINATION_EVIDENCE_DIR)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("1 Train → 2 Destination → 3 Review"));
    assert!(wide.contains("reachable destinations"));
    assert!(wide.contains("Route inspector"));
    assert!(wide.contains("Origin"));
    assert!(wide.contains("Directional demand"));
    assert!(wide.contains("Path"));
    assert!(wide.contains("Distance"));
    let (row, column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Oakridge").map(|column| (row, column)))
        .expect("the selected reachable destination is visibly marked");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column).expect("test terminal fits u16"),
            u16::try_from(row).expect("test terminal fits u16"),
        ),
        Some((theme::BACKGROUND, theme::ACCENT)),
    );
    fs::write(
        Path::new(DESTINATION_EVIDENCE_DIR).join("destination-chooser-120x40.txt"),
        &wide,
    )?;

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact.contains("Route inspector"));
    assert!(compact.contains("Path:"));
    fs::write(
        Path::new(DESTINATION_EVIDENCE_DIR).join("destination-chooser-80x24.txt"),
        &compact,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Left),
        ShellAction::Continue
    );
    let train_step = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(train_step.contains("available Fleet"));
    assert!(train_step.contains("> Train 01"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("Passenger Service"));
    assert!(state.player_company.passenger_services.is_empty());
    assert!(state.active_journeys.is_empty());
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        state, before,
        "a route preview creates no Service or Journey"
    );

    let mut changed = state.clone();
    let mut changed_shell = Shell::new();
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Char('d')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Enter),
        ShellAction::Continue
    );
    changed.player_company.fleet.trains[0].status = TrainStatus::Travelling {
        journey_id: railq::model::JourneyId::new(99),
    };
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Enter),
        ShellAction::Continue
    );
    let unavailable = capture_rendered_buffer_mut(&mut changed_shell, &changed, 120, 40);
    assert!(unavailable.contains("no longer READY"));
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Backspace),
        ShellAction::Continue
    );
    let recovered = capture_rendered_buffer_mut(&mut changed_shell, &changed, 120, 40);
    assert!(recovered.contains("Train 02"));
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut changed_shell, &changed, KeyCode::Enter),
        ShellAction::Continue
    );
    assert!(
        capture_rendered_buffer_mut(&mut changed_shell, &changed, 120, 40)
            .contains("reachable destinations"),
        "the remaining READY Train can resume the flow"
    );
    Ok(())
}

#[test]
fn chooser_pages_through_a_long_ready_fleet() {
    let mut state = create_new_game(42, "Long Fleet", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    for _ in 0..12 {
        purchase_train(&mut state, 0, RailStationId::new(1)).expect("Train is purchased");
    }
    let before = state.clone();
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let initial = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(initial.contains("> Train 01"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let paged = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(paged.contains("> Train 11"));
    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(state, before, "scrolling is presentation-only");
}

#[test]
fn chooser_never_substitutes_a_changed_train_id_and_empty_fleet_explains_the_next_action() {
    let mut state = state_with_ready_trains();
    let mut shell = Shell::new();
    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );

    let first_train_id = state.player_company.fleet.trains[0].id;
    let second_train_id = state.player_company.fleet.trains[1].id;
    state.player_company.fleet.trains[0].status = TrainStatus::Travelling {
        journey_id: railq::model::JourneyId::new(99),
    };
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let changed = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(changed.contains("previously selected Train is no longer READY"));
    assert!(
        !changed.contains("> Train 02"),
        "a different Train is not silently selected"
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::ManualDispatch {
            train_id: second_train_id,
            destination_station_id: RailStationId::new(1),
        }
    );
    assert_ne!(first_train_id, second_train_id);

    let empty = create_new_game(42, "Empty Fleet", STARTED_AT);
    let mut empty_shell = Shell::new();
    assert_eq!(
        press(&mut empty_shell, &empty, KeyCode::Char('d')),
        ShellAction::Continue
    );
    let rendered = capture_rendered_buffer(&empty_shell, &empty, 120, 40);
    assert!(rendered.contains("No READY Train in the Fleet. Press B to buy a Train."));
}

#[test]
fn quote_review_groups_departure_and_arrival_terms_with_an_occupancy_gauge()
-> Result<(), Box<dyn Error>> {
    let state = state_with_ready_trains();
    let before = state.clone();
    let mut shell = Shell::new();
    fs::create_dir_all(QUOTE_EVIDENCE_DIR)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('d')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    for text in [
        "Manual Dispatch · Journey quote",
        "Occupancy",
        "boarded /",
        "DEPARTURE · paid now",
        "Infrastructure Access Fee",
        "Fuel Cost",
        "Paid-now total",
        "Funds after departure",
        "ARRIVAL · credited when the Journey arrives",
        "Arrival revenue",
        "Estimated profit",
        "revalidate and confirm",
    ] {
        assert!(wide.contains(text), "wide quote must contain {text:?}");
    }
    fs::write(
        Path::new(QUOTE_EVIDENCE_DIR).join("journey-quote-120x40.txt"),
        &wide,
    )?;

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for text in [
        "Occupancy",
        "Paid-now total",
        "Funds after departure",
        "Arrival revenue",
        "Estimated profit",
    ] {
        assert!(
            compact.contains(text),
            "compact quote must contain {text:?}"
        );
    }
    fs::write(
        Path::new(QUOTE_EVIDENCE_DIR).join("journey-quote-80x24.txt"),
        compact,
    )?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(state, before, "review and cancellation must not dispatch");
    Ok(())
}

#[test]
fn quote_confirmation_refreshes_changed_terms_and_keeps_insufficient_funds_rejection_open() {
    let mut state = state_with_ready_trains();
    let mut shell = Shell::new();
    let train_id = state.player_company.fleet.trains[0].id;

    press(&mut shell, &state, KeyCode::Char('d'));
    press(&mut shell, &state, KeyCode::Enter);
    press(&mut shell, &state, KeyCode::Enter);
    state
        .origin_destination_demand
        .iter_mut()
        .find(|demand| {
            demand.origin_station_id == RailStationId::new(1)
                && demand.destination_station_id == RailStationId::new(2)
        })
        .expect("selected route has directional demand")
        .waiting_passengers = 0;
    let after_external_change = state.clone();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue,
        "a changed quote must be reviewed before it reaches the application boundary"
    );
    let refreshed = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(refreshed.contains("Journey quote updated from current conditions"));
    assert!(refreshed.contains("0 boarded /"));
    assert!(refreshed.contains("EMPTY REPOSITIONING"));
    assert_eq!(
        state, after_external_change,
        "refreshing a quote changes no game state"
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::ManualDispatch {
            train_id,
            destination_station_id: RailStationId::new(2),
        }
    );

    let mut insufficient = state_with_ready_trains();
    insufficient.player_company.funds = Money::ZERO;
    let before_rejection = insufficient.clone();
    let mut insufficient_shell = Shell::new();
    press(&mut insufficient_shell, &insufficient, KeyCode::Char('d'));
    press(&mut insufficient_shell, &insufficient, KeyCode::Enter);
    press(&mut insufficient_shell, &insufficient, KeyCode::Enter);
    let review = capture_rendered_buffer_mut(&mut insufficient_shell, &insufficient, 120, 40);
    assert!(review.contains("INSUFFICIENT FUNDS"));

    insufficient_shell.reject_manual_dispatch(
        "Company Funds of 0 cents cannot cover Journey departure costs of 150 cents",
    );
    let rejected = capture_rendered_buffer_mut(&mut insufficient_shell, &insufficient, 120, 40);
    assert!(rejected.contains("Departure review: Company Funds of 0 cents"));
    assert_eq!(
        press(&mut insufficient_shell, &insufficient, KeyCode::Left),
        ShellAction::Continue
    );
    assert!(
        capture_rendered_buffer_mut(&mut insufficient_shell, &insufficient, 120, 40)
            .contains("reachable destinations"),
        "the rejected proposal retains its selected Train and destination path"
    );
    assert_eq!(
        press(&mut insufficient_shell, &insufficient, KeyCode::Esc),
        ShellAction::Continue
    );
    assert_eq!(
        insufficient, before_rejection,
        "an insufficient-funds rejection creates no Service or Journey"
    );
}
