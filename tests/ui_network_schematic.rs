//! Rendered-buffer coverage for the Rail Network schematic.

use std::{error::Error, fs, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use railq::{
    model::UtcSeconds,
    sim::world::create_new_game,
    ui::{Shell, ShellAction, capture_rendered_buffer_mut, capture_rendered_cell_colors, theme},
};

const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_700_000_000);
const EVIDENCE_DIR: &str = "tmp/ui-ux-plan/evidence/10";

fn press(shell: &mut Shell, state: &railq::model::GameState, code: KeyCode) {
    assert_eq!(
        shell.handle_key(KeyEvent::new(code, KeyModifiers::NONE), state),
        ShellAction::Continue
    );
}

#[test]
fn schematic_follows_station_selection_and_retains_rail_line_details() -> Result<(), Box<dyn Error>>
{
    let state = create_new_game(42, "Northstar Passenger", STARTED_AT);
    let before_presentation = state.clone();
    let mut shell = Shell::new();
    let evidence_dir = Path::new(EVIDENCE_DIR);
    fs::create_dir_all(evidence_dir)?;

    let wide = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(wide.contains("Rail Network · schematic"));
    for station in &state.region.rail_authority.rail_network.rail_stations {
        let settlement = state
            .region
            .settlements
            .iter()
            .find(|settlement| settlement.id == station.settlement_id)
            .expect("every Rail Station has a Settlement");
        assert!(wide.contains(&format!("[{:02}] {}", station.id.get(), settlement.name)));
    }
    assert!(wide.contains("Incident Rail Lines (1)"));
    assert!(wide.contains("Rail Line 01"));
    assert!(wide.contains("10 km"));
    let (selected_row, selected_column) = wide
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("[>] [01]").map(|column| (row, column)))
        .expect("the selected station must have a schematic marker");
    assert_eq!(
        capture_rendered_cell_colors(
            &shell,
            &state,
            120,
            40,
            u16::try_from(selected_column).expect("test terminal fits u16"),
            u16::try_from(selected_row).expect("test terminal fits u16"),
        ),
        Some((theme::ACCENT, theme::PANEL)),
    );
    fs::write(evidence_dir.join("network-schematic-120x40.txt"), wide)?;

    press(&mut shell, &state, KeyCode::Down);
    let selected_branch = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
    assert!(selected_branch.contains("[>] [02]"));
    assert!(selected_branch.contains("Incident Rail Lines (3)"));
    fs::write(
        evidence_dir.join("network-schematic-selected-120x40.txt"),
        selected_branch,
    )?;
    let compact = capture_rendered_buffer_mut(&mut shell, &state, 80, 24);
    assert!(!compact.contains("Rail Network · schematic"));
    assert!(compact.contains("Rail Stations · connected"));
    fs::write(
        evidence_dir.join("network-schematic-fallback-80x24.txt"),
        compact,
    )?;
    assert_eq!(
        state, before_presentation,
        "Map browsing must not mutate the Rail Network"
    );
    Ok(())
}

#[test]
fn diagram_is_stable_across_seeds_and_long_labels_fall_back_to_station_list()
-> Result<(), Box<dyn Error>> {
    for seed in [1_u64, 42, 99] {
        let state = create_new_game(seed, "Seeded Passenger", STARTED_AT);
        let mut shell = Shell::new();
        let rendered = capture_rendered_buffer_mut(&mut shell, &state, 120, 40);
        assert!(rendered.contains("Rail Network · schematic"));
        for station in &state.region.rail_authority.rail_network.rail_stations {
            assert!(rendered.contains(&format!("[{:02}]", station.id.get())));
        }
    }

    let mut long_name_state = create_new_game(42, "Long-name Passenger", STARTED_AT);
    long_name_state.region.settlements[0].name =
        "Westborough-and-the-Upper-Valley-Terminal-Interchange-and-Depot".into();
    let mut shell = Shell::new();
    let compact_workspace = capture_rendered_buffer_mut(&mut shell, &long_name_state, 120, 40);
    assert!(!compact_workspace.contains("Rail Network · schematic"));
    assert!(compact_workspace.contains("Rail Stations · connected"));
    fs::create_dir_all(EVIDENCE_DIR)?;
    fs::write(
        Path::new(EVIDENCE_DIR).join("network-long-label-fallback-120x40.txt"),
        compact_workspace,
    )?;
    Ok(())
}
