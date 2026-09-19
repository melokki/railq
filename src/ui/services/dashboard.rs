//! Passenger Service dashboard summary surfaces.
//!
//! Keep the aggregate network signals separate from the selected Service
//! inspector so this workspace answers "how are Services operating?" before
//! the player drills into one route pattern.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
};

use crate::{
    model::{GameState, ServiceId},
    ui::components,
};

use super::service_operating_snapshot;

pub(super) fn render_metrics(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_service_id: Option<ServiceId>,
) {
    let [network_area, allocation_area, load_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    let services = &state.player_company.passenger_services;
    let mut assigned = 0usize;
    let mut ready = 0usize;
    let mut running = 0usize;
    for service in services {
        let snapshot = service_operating_snapshot(state, service.id);
        assigned = assigned.saturating_add(snapshot.assigned_trains);
        ready = ready.saturating_add(snapshot.runnable_assigned_trains);
        running = running.saturating_add(snapshot.active_trains);
    }

    components::render_metric_card(
        frame,
        network_area,
        "SERVICE NETWORK",
        format!(
            "{} {}",
            services.len(),
            plural(services.len(), "service", "services")
        ),
        format!("{running} running"),
        format!("{ready} ready to depart"),
    );

    let fleet_size = state.player_company.fleet.trains.len();
    let unassigned = fleet_size.saturating_sub(assigned);
    components::render_metric_card(
        frame,
        allocation_area,
        "FLEET ALLOCATION",
        format!("{assigned} assigned"),
        format!("{unassigned} unassigned"),
        format!(
            "{fleet_size} {} in fleet",
            plural(fleet_size, "train", "trains")
        ),
    );

    let Some(snapshot) = selected_service_id
        .map(|service_id| service_operating_snapshot(state, service_id))
    else {
        components::render_metric_card(
            frame,
            load_area,
            "PASSENGER LOAD",
            "—".into(),
            "no service selected".into(),
            String::new(),
        );
        return;
    };

    let onboard = if snapshot.total_capacity > 0 {
        format!(
            "{} / {} onboard",
            snapshot.onboard_passengers, snapshot.total_capacity
        )
    } else {
        format!("{} onboard", snapshot.onboard_passengers)
    };
    let context = if snapshot.total_capacity > 0 {
        let occupancy = u64::from(snapshot.onboard_passengers).saturating_mul(100)
            / u64::from(snapshot.total_capacity);
        format!("{occupancy}% selected load")
    } else if snapshot.assigned_trains > 0 {
        "selected service not running".into()
    } else {
        "no train assigned".into()
    };
    components::render_metric_card(
        frame,
        load_area,
        "PASSENGER LOAD",
        format!("{} waiting", snapshot.waiting_passengers),
        onboard,
        context,
    );
}

const fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 {
        singular
    } else {
        plural
    }
}
