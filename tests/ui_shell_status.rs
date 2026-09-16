//! Shell status and map chrome behavior tests.

use std::error::Error;

use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    ui::{Shell, capture_rendered_buffer},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);

#[test]
fn captures_company_status_at_wide_and_compact_sizes() -> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    let travelling_train = purchase_train(&mut state, 0, RailStationId::new(1))?;
    purchase_train(&mut state, 0, RailStationId::new(1))?;
    let service = find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))?;
    dispatch_journey(&mut state, travelling_train, service, STARTED_AT)?;

    let shell = Shell::new();
    for (columns, rows) in [(120, 40), (80, 24)] {
        let rendered = capture_rendered_buffer(&shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("Funds"));
        assert!(rendered.contains("ETA"));
    }
    Ok(())
}

#[test]
fn map_header_separates_registration_from_the_marker_legend() {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let shell = Shell::new();
    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);
    let registration = format!(
        "{} {}",
        state.region.railway_registration.display_code(),
        state.region.railway_registration.mark
    );

    assert!(rendered.contains("Network"));
    assert!(rendered.contains("Registration ·"));
    assert!(rendered.contains(&registration));
    let selected_station = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .first()
        .expect("starter world has a selected station");
    let selected_name = state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == selected_station.settlement_id)
        .expect("selected station belongs to a settlement")
        .name
        .as_str();

    // Selection identity belongs to the map marker/label and inspector; the
    // border no longer repeats it as a second focus caption.
    assert!(!rendered.contains(&format!("◆ {selected_name}")));
    assert!(rendered.contains("● station"));
    assert!(rendered.contains("○ settlement"));
    assert!(rendered.contains("▶ train"));
    assert!(!rendered.contains("● connected  ○ unconnected  ▶ travelling"));
}

#[test]
fn map_station_inspector_uses_operational_sections_without_embedded_shortcuts() {
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let shell = Shell::new();
    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);

    assert!(rendered.contains("OPERATIONS"));
    assert!(rendered.contains("Ready here"));
    assert!(rendered.contains("Services"));
    assert!(rendered.contains("PASSENGERS"));
    assert!(rendered.contains("Waiting"));
    assert!(rendered.contains("Arrival rate"));
    assert!(!rendered.contains("DIRECT LINKS"));
    assert!(!rendered.contains("Direct links"));
    assert!(!rendered.contains("d Dispatch · all READY Trains"));
}

#[test]
fn map_footer_contains_actions_without_repeating_header_status() -> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    purchase_train(&mut state, 0, RailStationId::new(1))?;
    let shell = Shell::new();
    let rendered = capture_rendered_buffer(&shell, &state, 120, 40);

    assert!(rendered.contains("[D] Dispatch"));
    assert!(!rendered.contains("Dispatch · 1 ready"));
    Ok(())
}
