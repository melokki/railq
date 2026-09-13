//! Deterministic visual evidence for task 02's control-room shell.

use std::{error::Error, fs, path::Path};

use railq::{
    model::{Money, RailStationId, UtcSeconds},
    sim::{
        fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
        world::create_new_game,
    },
    ui::{Shell, capture_rendered_buffer},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/02";

#[test]
fn captures_company_status_at_wide_and_compact_sizes() -> Result<(), Box<dyn Error>> {
    let mut state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    state.player_company.funds = Money::from_cents(1_000_000);
    let travelling_train = purchase_train(&mut state, 0, RailStationId::new(1))?;
    purchase_train(&mut state, 0, RailStationId::new(1))?;
    let service = find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))?;
    dispatch_journey(&mut state, travelling_train, service, STARTED_AT)?;

    let shell = Shell::new();
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;
    for (columns, rows, file_name) in [
        (120, 40, "company-status-120x40.txt"),
        (80, 24, "company-status-80x24.txt"),
    ] {
        let rendered = capture_rendered_buffer(&shell, &state, columns, rows);
        assert_eq!(rendered.lines().count(), usize::from(rows));
        assert!(rendered.contains("Funds"));
        assert!(rendered.contains("ETA"));
        fs::write(evidence_dir.join(file_name), rendered)?;
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

    assert!(rendered.contains("Connected"));
    assert!(rendered.contains("OPERATIONS"));
    assert!(rendered.contains("Ready trains"));
    assert!(rendered.contains("Services"));
    assert!(rendered.contains("PASSENGERS"));
    assert!(rendered.contains("Waiting"));
    assert!(rendered.contains("Arrival rate"));
    assert!(rendered.contains("DIRECT LINKS"));
    assert!(!rendered.contains("d Dispatch · all READY Trains"));
}
