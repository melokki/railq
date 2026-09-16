//! Shell-level modal and status overlays.
//!
//! These presentation-only surfaces are shared across primary workspaces but do
//! not participate in input routing or application command execution.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::model::GameState;

use super::{format, modal, theme};

/// Presentation-only record of a command which crossed the save boundary.
/// It is deliberately not saved: reopening a game must not invent old notices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActionOutcome {
    pub(super) title: &'static str,
    pub(super) summary: String,
    pub(super) details: Vec<String>,
}

pub(super) fn render_world_details_overlay(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &GameState,
) {
    let card = modal::centered_rect(area, 86, 24);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "World Details",
        modal::shortcut_line(&[("Esc/W", "close")]),
    );

    let region = &state.region;
    let registration = &region.railway_registration;
    let network = &region.rail_authority.rail_network;
    let connected = region
        .settlements
        .iter()
        .filter(|settlement| {
            network
                .rail_stations
                .iter()
                .any(|station| station.settlement_id == settlement.id)
        })
        .count();
    let total = region.settlements.len();
    let coverage_percent = if total == 0 {
        0
    } else {
        connected.saturating_mul(100) / total
    };
    let network_metres = network.rail_lines.iter().fold(0_u64, |total, line| {
        total.saturating_add(line.distance.metres())
    });
    let territory = region
        .name
        .rsplit_once(" of ")
        .map_or(region.name.as_str(), |(_, territory)| territory);

    let lines = vec![
        world_section("REGION"),
        world_field("Region", &region.name),
        world_field("Rail Authority", &region.rail_authority.name),
        world_field("Population", &grouped_u64(region.population)),
        Line::from(""),
        world_section("RAILWAY REGISTRATION"),
        world_field(
            "Identity",
            &format!("{} · {}", registration.display_code(), registration.mark),
        ),
        world_field(
            &registration.display_code(),
            "fictional numeric railway registration code for this Region",
        ),
        world_field(
            &registration.mark,
            &format!("fictional two-letter railway mark assigned to {territory}"),
        ),
        Line::styled(
            "Used in official RailQ EVNs; it remains stable for this world.",
            theme::secondary(),
        ),
        Line::from(""),
        world_section("NETWORK"),
        world_field("Settlements", &format!("{total}")),
        world_field(
            "Connected",
            &format!("{connected} / {total} ({coverage_percent}%)"),
        ),
        world_field("Rail stations", &format!("{}", network.rail_stations.len())),
        world_field("Rail lines", &format!("{}", network.rail_lines.len())),
        world_field("Rail network", &format::distance(network_metres)),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        modal_areas.body,
    );
}

fn world_section(label: &str) -> Line<'static> {
    Line::styled(label.to_owned(), theme::focused_title())
}

fn world_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), theme::secondary()),
        Span::styled(value.to_owned(), theme::primary_value()),
    ])
}

fn grouped_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

pub(super) fn render_outcome_overlay(
    frame: &mut ratatui::Frame,
    area: Rect,
    outcome: &ActionOutcome,
) {
    let compact = area.width < 96 || area.height < 26;
    let overlay_area = if compact {
        area
    } else {
        Rect::new(
            area.x.saturating_add(area.width / 10),
            area.y.saturating_add(area.height / 5),
            area.width.saturating_mul(4) / 5,
            area.height.saturating_mul(3) / 5,
        )
    };
    let mut lines = vec![
        Line::styled(&outcome.summary, theme::success()),
        Line::from(""),
    ];
    lines.extend(outcome.details.iter().cloned().map(Line::from));
    lines.push(Line::from(""));
    lines.push(Line::styled("i / Esc · close details", theme::hint()));
    frame.render_widget(Clear, overlay_area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(theme::THIN_BORDERS)
                    .border_style(theme::focused_border())
                    .title(outcome.title)
                    .title_style(theme::focused_title())
                    .style(theme::panel()),
            )
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        overlay_area,
    );
}

pub(super) fn render_bankruptcy_restart_confirmation(
    frame: &mut ratatui::Frame,
    area: Rect,
) {
    let card = modal::centered_rect(area, 70, 15);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "Confirm Safe Restart",
        modal::shortcut_line(&[("Enter", "restart"), ("Esc", "keep save")]),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Safe restart review", theme::focused_title()),
            Line::from(""),
            Line::from(
                "A fresh game is created only after the current Player Company save is preserved in a unique archive backup.",
            ),
            Line::from(""),
            Line::styled(
                "The existing save is never silently overwritten.",
                theme::warning(),
            ),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

pub(super) fn bankruptcy_text(restart_confirmation: bool) -> String {
    if restart_confirmation {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "",
            "Safe restart review is open. A fresh game is created only after this Player Company save is preserved in a unique archive backup.",
        ]
        .join("\n")
    } else {
        [
            "[X] BANKRUPTCY",
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
            "Normal operations are disabled. A safe restart preserves this Company save in an archive backup before creating a fresh game.",
        ]
        .join("\n")
    }
}
