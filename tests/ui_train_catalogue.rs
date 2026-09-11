//! Rendered-buffer and keyboard coverage for delivery Rail Station selection.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::{RailStation, RailStationId, Settlement, SettlementId, UtcSeconds},
    sim::world::create_new_game,
    ui::{
        Shell, ShellAction, capture_rendered_buffer_mut, capture_rendered_cell_colors,
        market::MarketFlow, theme,
    },
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/18";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) -> ShellAction {
    shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state)
}

#[test]
fn delivery_station_list_preserves_model_choice_and_reaches_existing_review()
-> Result<(), Box<dyn Error>> {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let before = state.clone();
    let mut shell = Shell::new();
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    for expected in [
        "Local 70",
        "Express 120",
        "$3,000.00",
        "$5,000.00",
        "70 pax",
        "120 pax",
        "90.0 km/h",
        "118.8 km/h",
        "$0.45/km",
        "$0.30/km",
        "Selected model",
    ] {
        assert!(
            wide.contains(expected),
            "wide catalogue should show {expected}"
        );
    }

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for expected in [
        "Local 70",
        "Express 120",
        "$3,000.00",
        "$5,000.00",
        "70 pax",
        "120 pax",
        "90.0 km/h",
        "118.8 km/h",
        "$0.45/km",
        "$0.30/km",
    ] {
        assert!(
            compact.contains(expected),
            "80x24 catalogue should retain {expected}"
        );
    }

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    let selected = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    let (row, column) = selected
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Express 120").map(|column| (row, column)))
        .expect("the selected catalogue model is visibly marked");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column)?,
            u16::try_from(row)?,
        ),
        Some((theme::BACKGROUND, theme::ACCENT))
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let delivery = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    for expected in [
        "1 Train → 2 Delivery Rail Station → 3 Review",
        "Delivery Rail Stations",
        "Selected delivery",
        "Express 120",
        "Left / Backspace · model",
    ] {
        assert!(
            delivery.contains(expected),
            "delivery view should show {expected}"
        );
    }
    for station in &state.region.rail_authority.rail_network.rail_stations {
        let name = state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == station.settlement_id)
            .expect("generated Rail Stations have Settlements")
            .name
            .as_str();
        assert!(delivery.contains(name), "delivery list should show {name}");
    }
    for settlement in &state.region.settlements {
        let has_rail_station = state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .iter()
            .any(|station| station.settlement_id == settlement.id);
        if !has_rail_station {
            assert!(
                !delivery.contains(&settlement.name),
                "unconnected Settlement {} must not be a delivery choice",
                settlement.name
            );
        }
    }
    let (row, column) = delivery
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("> Pinewatch").map(|column| (row, column)))
        .expect("the selected delivery Rail Station is visibly marked");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(column)?,
            u16::try_from(row)?,
        ),
        Some((theme::BACKGROUND, theme::ACCENT))
    );
    fs::write(evidence_dir.join("delivery-stations-120x40.txt"), &delivery)?;

    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    for expected in [
        "Delivery Rail Stations",
        "1 Train → 2 Delivery Rail Station → 3 Review",
        "Enter · review",
        "Left · model",
    ] {
        assert!(
            compact.contains(expected),
            "compact delivery view should show {expected}"
        );
    }
    fs::write(evidence_dir.join("delivery-stations-80x24.txt"), compact)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Down),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let review = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(review.contains("Deliver Express 120 to"));
    assert!(review.contains("Enter confirms purchase (revalidated)"));
    fs::write(evidence_dir.join("delivery-review-120x40.txt"), review)?;

    assert_eq!(
        press(&mut shell, &state, KeyCode::Esc),
        ShellAction::Continue
    );

    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Backspace),
        ShellAction::Continue
    );
    let restored = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(restored.contains("> Express 120"));
    assert_eq!(
        state, before,
        "delivery selection, review, and cancellation are presentation-only"
    );
    Ok(())
}

#[test]
fn delivery_selection_scrolls_and_recovers_when_its_station_disappears() {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    for index in 5_u64..=24 {
        let settlement_id = SettlementId::new(index + 100);
        state.region.settlements.push(Settlement {
            id: settlement_id,
            name: format!("Long Delivery Station {index}"),
            population: 1_000,
        });
        state
            .region
            .rail_authority
            .rail_network
            .rail_stations
            .push(RailStation {
                id: RailStationId::new(index + 100),
                settlement_id,
            });
    }
    let mut shell = Shell::new();

    assert_eq!(
        press(&mut shell, &state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::Enter),
        ShellAction::Continue
    );
    let _ = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut shell, &state, KeyCode::PageDown),
        ShellAction::Continue
    );
    let scrolled = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(
        scrolled.contains("> Long Delivery Station 24"),
        "the selected delivery Rail Station must scroll into view"
    );

    let mut stale_state = create_new_game(42, "Stale Delivery", STARTED_AT);
    let mut stale_shell = Shell::new();
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Char('b')),
        ShellAction::Continue
    );
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Enter),
        ShellAction::Continue
    );
    stale_state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .remove(0);
    assert_eq!(
        press(&mut stale_shell, &stale_state, KeyCode::Enter),
        ShellAction::Continue
    );
    let stale = capture_rendered_buffer_mut(&mut stale_shell, &stale_state, 120, 40);
    assert!(stale.contains("previously selected delivery Rail Station is no longer available"));
    assert!(stale.contains("Delivery Rail Stations"));

    let mut missing_state = create_new_game(42, "Missing Delivery", STARTED_AT);
    missing_state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .clear();
    assert_eq!(
        MarketFlow::start(&missing_state, 0).unwrap_err(),
        "No connected Rail Station is available for delivery."
    );
}
