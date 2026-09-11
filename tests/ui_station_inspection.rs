//! Keyboard and rendered-buffer coverage for Map Rail Station inspection.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{fleet::purchase_train, world::create_new_game},
    ui::{
        Shell, ShellAction, capture_rendered_buffer, capture_rendered_buffer_mut,
        capture_rendered_cell_colors, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/09";
const SETTLEMENT_EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/11";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

fn state_with_ready_trains() -> railq::model::GameState {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(10_000_000);
    purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
    purchase_train(&mut state, 0, RailStationId::new(2)).unwrap();
    state
}

#[test]
fn station_list_selects_stable_ids_and_drives_ready_train_and_directional_demand_inspection()
-> Result<(), Box<dyn Error>> {
    let mut state = state_with_ready_trains();
    let zero_pool = state
        .origin_destination_demand
        .iter_mut()
        .find(|pool| {
            pool.origin_station_id == RailStationId::new(1)
                && pool.destination_station_id == RailStationId::new(2)
        })
        .expect("starter Rail Network has every directional market");
    zero_pool.waiting_passengers = 0;
    let before_presentation = state.clone();
    let mut shell = Shell::new();

    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Rail Stations · connected"));
    assert!(wide.contains("> [01]"));
    assert!(wide.contains("Rail Station 01"));
    assert!(wide.contains("Ready Trains (1)"));
    assert!(wide.contains("Directional Waiting Passengers"));
    assert!(wide.contains("0 waiting"));
    let (selected_row, selected_column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> [01]").map(|column| (row, column)))
        .expect("selected Rail Station must be visible");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(selected_column).expect("test terminal fits u16"),
            u16::try_from(selected_row).expect("test terminal fits u16"),
        ),
        Some((theme::BACKGROUND, theme::ACCENT)),
    );
    fs::write(evidence_dir.join("station-inspector-120x40.txt"), wide)?;

    for expected_id in [2_u64, 3, 4] {
        press(&mut shell, &state, KeyCode::Down);
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
        assert!(rendered.contains(&format!("Rail Station {expected_id:02}")));
        assert!(rendered.contains("Directional Waiting Passengers"));
    }
    assert_eq!(
        state, before_presentation,
        "browsing must be presentation-only"
    );
    Ok(())
}

#[test]
fn compact_station_details_are_a_focused_page_and_unavailable_demand_is_not_zero()
-> Result<(), Box<dyn Error>> {
    let mut state = state_with_ready_trains();
    state.origin_destination_demand.retain(|pool| {
        !(pool.origin_station_id == RailStationId::new(1)
            && pool.destination_station_id == RailStationId::new(3))
    });
    let before_presentation = state.clone();
    let mut shell = Shell::new();

    let compact_list = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_list.contains("Rail Stations · connected"));
    assert!(!compact_list.contains("Directional Waiting Passengers"));

    press(&mut shell, &state, KeyCode::Enter);
    let compact_details = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_details.contains("Rail Station 01"));
    assert!(compact_details.contains("Directional Waiting Passengers"));
    assert!(compact_details.contains("Waiting Passengers unavailable"));
    fs::write(
        Path::new(EVIDENCE_DIR).join("station-inspector-80x24.txt"),
        compact_details,
    )?;

    press(&mut shell, &state, KeyCode::Esc);
    let returned_list = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(returned_list.contains("Rail Stations · connected"));
    assert!(!returned_list.contains("Directional Waiting Passengers"));

    press(&mut shell, &state, KeyCode::Char('d'));
    let dispatch = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(dispatch.contains("Manual Dispatch"));
    assert_eq!(
        state, before_presentation,
        "Map input must not change game state"
    );
    Ok(())
}

#[test]
fn map_switches_to_unconnected_settlements_with_independent_selection_and_no_dispatch()
-> Result<(), Box<dyn Error>> {
    let state = state_with_ready_trains();
    let before_presentation = state.clone();
    let mut shell = Shell::new();
    fs::create_dir_all(SETTLEMENT_EVIDENCE_DIR)?;

    press(&mut shell, &state, KeyCode::Down);
    let station_two = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(station_two.contains("Rail Station 02"));

    press(&mut shell, &state, KeyCode::Tab);
    let settlements = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(settlements.contains("Unconnected Settlements · inspect-only"));
    assert!(settlements.contains("UNCONNECTED"));
    for settlement in state.region.settlements.iter().filter(|settlement| {
        !state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .any(|station| station.settlement_id == settlement.id)
    }) {
        assert!(settlements.contains(&settlement.name));
        assert!(settlements.contains(&format_population(settlement.population)));
    }
    fs::write(
        Path::new(SETTLEMENT_EVIDENCE_DIR).join("unconnected-settlements-120x40.txt"),
        &settlements,
    )?;

    press(&mut shell, &state, KeyCode::PageDown);
    let last_settlement = state
        .region
        .settlements
        .iter()
        .rev()
        .find(|settlement| {
            !state
                .region
                .rail_authority
                .rail_network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
        })
        .expect("seeded game has an unconnected Settlement");
    let compact_list = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(compact_list.contains(&format!("> [{:02}]", last_settlement.id.get())));
    fs::write(
        Path::new(SETTLEMENT_EVIDENCE_DIR).join("unconnected-settlements-80x24.txt"),
        &compact_list,
    )?;

    press(&mut shell, &state, KeyCode::Enter);
    let details = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(details.contains("Connection status"));
    assert!(details.contains("No Rail Station"));
    assert!(details.contains("Inspection only"));
    assert!(!details.contains("Directional Waiting Passengers"));

    press(&mut shell, &state, KeyCode::Char('d'));
    let rejected_dispatch = capture_rendered_buffer(&shell, &state, 80, 24);
    assert!(rejected_dispatch.contains("dispatch/construction unavailable"));
    assert!(!rejected_dispatch.contains("Manual Dispatch"));

    press(&mut shell, &state, KeyCode::Esc);
    press(&mut shell, &state, KeyCode::Tab);
    let journeys = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(journeys.contains("Departure Board · active Journeys"));
    press(&mut shell, &state, KeyCode::Tab);
    let restored_station = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored_station.contains("Rail Station 02"));
    assert_eq!(
        state, before_presentation,
        "switching and inspecting must remain presentation-only"
    );
    Ok(())
}

fn format_population(population: u64) -> String {
    let digits = population.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}
