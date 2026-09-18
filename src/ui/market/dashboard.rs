//! Market overview surfaces for rolling-stock procurement.
//!
//! These summaries keep the buying decision visible before the player drills
//! into catalogue specifications: what the selected model costs, what it adds,
//! what it costs to run, and whether the Company already owns it.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    catalog::TrainModel,
    model::GameState,
    ui::{components, theme},
};

use super::{
    catalogue_ownership, delivery_station_ids, format_money, format_money_per_kilometre,
    format_speed_kmh, funds_after_purchase_display, sample_trip,
};

pub(super) fn render_overview(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selected_train: Option<&TrainModel>,
) {
    let catalogue = crate::catalog::train_catalogue().models();
    let within_funds = catalogue
        .iter()
        .filter(|train| state.player_company.funds >= train.purchase_price())
        .count();
    let fleet_size = state.player_company.fleet.trains.len();

    let (status, status_style) = if delivery_station_ids(state).is_empty() {
        ("DELIVERY UNAVAILABLE", theme::warning())
    } else if within_funds == 0 {
        ("FUNDS CONSTRAINED", theme::error())
    } else {
        ("PROCUREMENT READY", theme::success())
    };
    let selected = selected_train
        .map(|train| format!(" · Selected {}", train.name()))
        .unwrap_or_default();

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(status, status_style.bold()),
            Span::styled(
                format!(
                    " · {} models · {within_funds} within funds · Fleet {fleet_size} trains{selected}",
                    catalogue.len()
                ),
                theme::secondary(),
            ),
        ]))
        .style(theme::panel()),
        area,
    );
}

pub(super) fn render_metrics(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    train: Option<&TrainModel>,
) {
    let [acquisition_area, capacity_area, operating_area, presence_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);

    let Some(train) = train else {
        for (card, title) in [
            (acquisition_area, "ACQUISITION"),
            (capacity_area, "CAPACITY"),
            (operating_area, "OPERATING COST"),
            (presence_area, "FLEET PRESENCE"),
        ] {
            components::render_metric_card(
                frame,
                card,
                title,
                "—".into(),
                "no model selected".into(),
                String::new(),
            );
        }
        return;
    };

    let acquisition_context = if state.player_company.funds >= train.purchase_price() {
        format!("{} cash after", funds_after_purchase_display(state, train))
    } else {
        let shortfall = train
            .purchase_price()
            .checked_sub(state.player_company.funds)
            .map(format_money)
            .unwrap_or_else(|_| "unavailable".into());
        format!("{shortfall} shortfall")
    };
    components::render_metric_card(
        frame,
        acquisition_area,
        "ACQUISITION",
        format_money(train.purchase_price()),
        "purchase price".into(),
        acquisition_context,
    );

    components::render_metric_card(
        frame,
        capacity_area,
        "CAPACITY",
        format!("{} seats", train.passenger_capacity().passengers()),
        "selected model".into(),
        format!("{} top speed", format_speed_kmh(train)),
    );

    let sample_context = sample_trip(state, train)
        .map(|sample| format!("{} benchmark trip", format_money(sample.departure_cost)))
        .unwrap_or_else(|| "benchmark trip unavailable".into());
    components::render_metric_card(
        frame,
        operating_area,
        "OPERATING COST",
        format!(
            "{}/km",
            format_money_per_kilometre(train.fuel_cost_per_kilometre().cents_per_kilometre())
        ),
        "fuel cost".into(),
        sample_context,
    );

    let ownership = catalogue_ownership(state, train);
    components::render_metric_card(
        frame,
        presence_area,
        "FLEET PRESENCE",
        format!("{} owned", ownership.owned),
        format!("{} ready · {} travelling", ownership.ready, ownership.travelling),
        if ownership.owned == 0 {
            "new model for this fleet".into()
        } else {
            "already represented".into()
        },
    );
}
