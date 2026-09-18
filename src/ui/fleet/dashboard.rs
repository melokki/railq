//! Fleet dashboard summary surfaces.
//!
//! Keep aggregate Fleet signals separate from Train inspection so the main
//! workspace can answer "how is the Fleet doing?" before the player drills
//! into one rolling-stock asset.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    catalog::model_for_train,
    model::{GameState, TrainStatus, UtcSeconds},
    ui::{components, theme},
};

use super::{nearest_arrival, resale_proceeds};

pub(super) fn render_overview(frame: &mut Frame, area: Rect, state: &GameState) {
    let total = state.player_company.fleet.trains.len();
    let ready = ready_train_count(state);
    let travelling = total.saturating_sub(ready);
    let assigned = assigned_train_count(state);

    let (status, status_style) = if ready > 0 {
        ("FLEET AVAILABLE", theme::success())
    } else {
        ("FLEET IN SERVICE", theme::focused_title())
    };
    let summary = format!(
        " · {total} {} · {assigned} assigned · {travelling} travelling",
        plural(total, "train", "trains"),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(status, status_style.bold()),
            Span::styled(summary, theme::secondary()),
        ]))
        .style(theme::panel()),
        area,
    );
}

pub(super) fn render_metrics(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
) {
    let [availability_area, allocation_area, capacity_area, value_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    let total = state.player_company.fleet.trains.len();
    let ready = ready_train_count(state);
    let travelling = total.saturating_sub(ready);
    let assigned = assigned_train_count(state);
    let unassigned = total.saturating_sub(assigned);
    let service_count = assigned_service_count(state);

    render_metric_card(
        frame,
        availability_area,
        "AVAILABILITY",
        format!("{ready} / {total} ready"),
        format!("{travelling} travelling"),
        if travelling == 0 {
            "all trains at stations".into()
        } else {
            nearest_arrival(state, now)
                .map(|eta| format!("next arrival in {eta}"))
                .unwrap_or_else(|| "arrival time unavailable".into())
        },
    );

    render_metric_card(
        frame,
        allocation_area,
        "ALLOCATION",
        format!("{assigned} assigned"),
        format!(
            "across {service_count} {}",
            plural(service_count, "service", "services")
        ),
        format!("{unassigned} unassigned"),
    );

    let (capacity, capacity_context) = match fleet_capacity(state) {
        Some(capacity) => {
            let average = capacity / u64::try_from(total).unwrap_or(1).max(1);
            (
                format!("{capacity} seats"),
                format!("{average} avg / train"),
            )
        }
        None => ("Unavailable".into(), "catalogue data incomplete".into()),
    };
    render_metric_card(
        frame,
        capacity_area,
        "SEAT CAPACITY",
        capacity,
        "owned capacity".into(),
        capacity_context,
    );

    let (resale_value, acquisition_value) = fleet_values(state);
    render_metric_card(
        frame,
        value_area,
        "ASSET VALUE",
        resale_value
            .map(format_cents)
            .unwrap_or_else(|| "Unavailable".into()),
        "resale value".into(),
        format!("{} acquisition cost", format_cents(acquisition_value)),
    );
}

fn render_metric_card(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: String,
    subtitle: String,
    context: String,
) {
    let block = components::panel_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(value, theme::primary_value().bold()),
            Line::styled(subtitle, theme::secondary()),
            Line::styled(context, theme::hint()),
        ])
        .alignment(Alignment::Center)
        .style(theme::panel()),
        inner,
    );
}

fn ready_train_count(state: &GameState) -> usize {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| matches!(train.status, TrainStatus::Ready { .. }))
        .count()
}

fn assigned_train_count(state: &GameState) -> usize {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .filter(|train| {
            state
                .player_company
                .fleet
                .assigned_service_id(train.id)
                .is_some()
        })
        .count()
}

fn assigned_service_count(state: &GameState) -> usize {
    state
        .player_company
        .passenger_services
        .iter()
        .filter(|service| {
            state.player_company.fleet.trains.iter().any(|train| {
                state.player_company.fleet.assigned_service_id(train.id) == Some(service.id)
            })
        })
        .count()
}

fn fleet_capacity(state: &GameState) -> Option<u64> {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .try_fold(0_u64, |capacity, train| {
            let seats = u64::from(model_for_train(train)?.passenger_capacity().passengers());
            capacity.checked_add(seats)
        })
}

fn fleet_values(state: &GameState) -> (Option<i128>, i128) {
    let acquisition_value = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| i128::from(train.original_purchase_price.cents()))
        .sum();
    let resale_value = state
        .player_company
        .fleet
        .trains
        .iter()
        .try_fold(0_i128, |value, train| {
            resale_proceeds(train.original_purchase_price)
                .map(|proceeds| value + i128::from(proceeds.cents()))
        });
    (resale_value, acquisition_value)
}

fn format_cents(cents: i128) -> String {
    crate::ui::format::signed_cents(cents)
        .trim_start_matches('+')
        .to_owned()
}

const fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train,
            services::{assign_train_to_service, find_or_create_service},
            world::create_new_game,
        },
    };

    use super::{assigned_service_count, assigned_train_count, fleet_capacity, fleet_values};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    #[test]
    fn aggregate_metrics_follow_owned_fleet_and_assignments() {
        let mut state = create_new_game(42, "Dashboard Passenger", STARTED_AT);
        state.player_company.funds = Money::from_cents(1_000_000);
        let first = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        let service =
            find_or_create_service(&mut state, RailStationId::new(1), RailStationId::new(2))
                .unwrap();
        assign_train_to_service(&mut state, first, service).unwrap();

        assert_eq!(assigned_train_count(&state), 1);
        assert_eq!(assigned_service_count(&state), 1);
        assert_eq!(fleet_capacity(&state), Some(140));
        assert_eq!(fleet_values(&state), (Some(420_000), 600_000));
    }
}
