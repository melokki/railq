//! Interaction and rendered-buffer coverage for retained Journey receipts.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{
        GameState, JourneyId, JourneyPurpose, JourneyReceipt, Money, PassengerService,
        ServiceDirectionMode, ServiceId, UtcSeconds,
    },
    sim::world::create_new_game,
    ui::{
        Shell, ShellAction, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/07";

fn receipt(id: u64) -> JourneyReceipt {
    JourneyReceipt {
        journey_id: JourneyId::new(id),
        revenue: Money::from_cents((id as i64) * 10_000),
        infrastructure_access_fee: Money::from_cents((id as i64) * 1_000),
        fuel_cost: Money::from_cents((id as i64) * 250),
        train_id: None,
        train_model_name: None,
        origin_station_id: None,
        destination_station_id: None,
        passengers_carried: None,
        passenger_capacity: None,
        completed_at: None,
        service_id: None,
        service_code: None,
        purpose: None,
        departed_at: None,
    }
}

fn retained_receipts_fixture() -> GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.financials.recent_journey_receipts = (1..=18).map(receipt).collect();
    state
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn company_shell(state: &GameState) -> Shell {
    let mut shell = Shell::new();
    assert_eq!(
        shell.handle_key(key(KeyCode::Char('4')), state),
        ShellAction::Continue
    );
    shell
}

#[test]
fn dashboard_keeps_history_out_of_the_main_view_and_history_preserves_selection()
-> Result<(), Box<dyn Error>> {
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let mut state = retained_receipts_fixture();
    let mut shell = company_shell(&state);

    let dashboard = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(dashboard.contains("RECENT ACTIVITY · LAST 5"));
    assert!(dashboard.contains("J18"));
    assert!(!dashboard.contains("J12"), "dashboard should only show the latest five receipts");
    assert!(!dashboard.contains("JOURNEY HISTORY"));
    assert!(dashboard.contains("[H] History"));
    assert!(!dashboard.contains("[↑↓/JK] Receipt"));
    fs::write(evidence_dir.join("dashboard-120x40.txt"), dashboard)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('h')), &state),
        ShellAction::Continue
    );
    let history = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(history.contains("Journey History · 18 receipts"));
    assert!(history.contains("J18"));
    assert!(history.contains("[Esc] close"));
    assert!(history.contains("[Enter] inspect"));
    assert!(history.contains("[↑↓/JK] scroll"));
    assert!(history.contains("[PgUp/PgDn] page"));
    let (selected_x, selected_y) =
        text_position(&history, "J18").expect("latest receipt should be selected in history");
    let selected_colors =
        capture_rendered_cell_colors(&shell, &state, 120, 40, selected_x, selected_y)
            .expect("selected receipt should paint its Journey cell");
    assert_eq!(selected_colors, (theme::BACKGROUND, theme::ACCENT));
    fs::write(evidence_dir.join("history-120x40.txt"), history)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::PageDown), &state),
        ShellAction::Continue
    );
    let scrolled = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(
        scrolled.contains("J01"),
        "PageDown must move the selection toward older receipts"
    );

    state.financials.recent_journey_receipts.push(receipt(19));
    assert_eq!(
        shell.handle_key(key(KeyCode::Enter), &state),
        ShellAction::Continue
    );
    let detail = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(detail.contains("Journey Receipt · J01"));
    assert!(detail.contains("Journey 1 · legacy"));
    assert!(detail.contains("$100.00"));
    assert!(detail.contains("$10.00"));
    assert!(detail.contains("$2.50"));
    assert!(detail.contains("+$87.50"));
    assert!(detail.contains("[Esc] close"));
    assert_eq!(
        capture_rendered_cell_colors(&shell, &state, 120, 40, 0, 0),
        Some((theme::MODAL_BACKDROP_TEXT, theme::MODAL_BACKDROP)),
        "Journey Receipt should mute the Company dashboard underneath it",
    );
    fs::write(evidence_dir.join("detail-120x40.txt"), detail)?;

    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    let restored_history = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored_history.contains("Journey History · 19 receipts"));
    assert!(
        restored_history.contains("> J01"),
        "Esc from a receipt must restore the selected history row"
    );

    assert_eq!(
        shell.handle_key(key(KeyCode::Esc), &state),
        ShellAction::Continue
    );
    let restored_dashboard = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored_dashboard.contains("RECENT ACTIVITY · LAST 5"));
    assert!(!restored_dashboard.contains("Journey History · 19 receipts"));

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('h')), &state),
        ShellAction::Continue
    );
    let compact_history = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_history.contains("Journey History · 19 receipts"));
    assert!(compact_history.contains("[Esc] close"));
    assert!(compact_history.contains("[Enter] inspect"));
    assert!(compact_history.contains("[↑↓] scroll"));
    fs::write(evidence_dir.join("history-80x24.txt"), compact_history)?;
    Ok(())
}

#[test]
fn wide_dashboard_aggregates_recent_service_performance() {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let station_ids = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .take(2)
        .map(|station| station.id)
        .collect::<Vec<_>>();
    assert_eq!(station_ids.len(), 2);
    let service_id = ServiceId::new(99);
    state.player_company.passenger_services.push(PassengerService {
        id: service_id,
        name: "R1".into(),
        custom_name: None,
        direction_mode: ServiceDirectionMode::BothDirections,
        forward_train_number: 101,
        reverse_train_number: Some(102),
        stop_station_ids: station_ids.clone(),
        rail_line_ids: Vec::new(),
    });

    let mut revenue = receipt(1);
    revenue.passengers_carried = Some(80);
    revenue.service_id = Some(service_id);
    revenue.service_code = Some("R1".into());
    revenue.purpose = Some(JourneyPurpose::RevenueService);
    let mut positioning = receipt(2);
    positioning.revenue = Money::ZERO;
    positioning.infrastructure_access_fee = Money::from_cents(500);
    positioning.fuel_cost = Money::from_cents(250);
    positioning.passengers_carried = Some(0);
    positioning.service_id = Some(service_id);
    positioning.service_code = Some("R1".into());
    positioning.purpose = Some(JourneyPurpose::Positioning);
    state.financials.recent_journey_receipts = vec![revenue, positioning];

    let mut shell = company_shell(&state);
    let dashboard = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);

    assert!(dashboard.contains("SERVICE PERFORMANCE · RECENT 2 JOURNEYS"));
    assert!(dashboard.contains("Service"));
    assert!(dashboard.contains("Runs"));
    assert!(dashboard.contains("Pos"));
    assert!(dashboard.contains("R1 ·"));
    assert!(dashboard.contains("$100.00"));
    assert!(dashboard.contains("$20.00"));
    assert!(dashboard.contains("+$80.00"));
    assert!(dashboard.contains("ATTENTION"));
    assert!(dashboard.contains("1 of the last 2 completed Journeys lost money."));
}

#[test]
fn empty_dashboard_explains_activity_and_history_remains_available() {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let mut shell = company_shell(&state);
    let dashboard = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);

    assert!(dashboard.contains("RECENT ACTIVITY"));
    assert!(dashboard.contains("No completed Journey activity yet."));
    assert!(dashboard.contains("ATTENTION"));
    assert!(dashboard.contains("No passenger Services are defined yet."));
    assert!(dashboard.contains("[H] History"));
    assert!(!dashboard.contains("JOURNEY HISTORY"));

    assert_eq!(
        shell.handle_key(key(KeyCode::Char('h')), &state),
        ShellAction::Continue
    );
    let history = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(history.contains("Journey History · 0 receipts"));
    assert!(history.contains("No retained Journey receipts yet."));
    assert!(history.contains("Operating Revenue"));
    assert!(history.contains("[Esc] close"));
    assert!(!history.contains("[Enter] inspect"));
}

fn text_position(rendered: &str, needle: &str) -> Option<(u16, u16)> {
    rendered.lines().enumerate().find_map(|(row, line)| {
        line.find(needle).map(|byte_index| {
            let column = line[..byte_index].chars().count();
            (column as u16, row as u16)
        })
    })
}
