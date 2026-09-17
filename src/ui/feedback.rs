//! Presentation feedback for player actions crossing the application/save boundary.
//!
//! This module owns the transient context captured before a command executes so
//! the shell can describe the durably saved result afterwards. None of this
//! state is persisted as part of the game.

use crate::{
    catalog::{model_for_train, train_catalogue},
    model::{GameState, Money, RailStationId, ServiceId, TrainId},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingAction {
    pub(super) label: String,
    pub(super) details: Vec<String>,
    pub(super) funds_before: Money,
}

pub(super) fn arrival_station_label(state: &GameState, station_id: RailStationId) -> String {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return format!("Rail Station {}", station_id.get());
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map(|settlement| format!("{} Rail Station", settlement.name))
        .unwrap_or_else(|| format!("Rail Station {}", station_id.get()))
}

pub(super) fn pending_dispatch(
    state: &GameState,
    train_id: TrainId,
    service_id: ServiceId,
) -> PendingAction {
    let model: String = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or_else(
            || "unknown model".into(),
            |train| {
                model_for_train(train)
                    .map(|model| model.name().to_owned())
                    .unwrap_or_else(|| format!("unknown model ({})", train.model_id.as_str()))
            },
        );
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == service_id);
    let service_name = service
        .map(|service| service.display_name())
        .unwrap_or_else(|| format!("Service {}", service_id.get()));
    let route = service
        .map(|service| {
            service
                .stop_station_ids
                .iter()
                .map(|station_id| arrival_station_label(state, *station_id))
                .collect::<Vec<_>>()
                .join(" → ")
        })
        .unwrap_or_else(|| "unknown route".into());

    PendingAction {
        label: format!("Manual Dispatch · Train {:02} ({model})", train_id.get()),
        details: vec![format!(
            "{service_name} · {route}. Full operating costs are paid at departure; passenger revenue is credited stop by stop."
        )],
        funds_before: state.player_company.funds,
    }
}

pub(super) fn pending_purchase(
    state: &GameState,
    catalogue_index: usize,
    delivery_station_id: RailStationId,
) -> PendingAction {
    let model: String = train_catalogue().models().get(catalogue_index).map_or_else(
        || "selected catalogue Train".into(),
        |train| train.name().into(),
    );
    PendingAction {
        label: format!("Train purchase · {model}"),
        details: vec![format!(
            "Delivered to Rail Station {} after the purchase was persisted.",
            delivery_station_id.get()
        )],
        funds_before: state.player_company.funds,
    }
}

pub(super) fn pending_resale(state: &GameState, train_id: TrainId) -> PendingAction {
    let model: String = state
        .player_company
        .fleet
        .trains
        .iter()
        .find(|train| train.id == train_id)
        .map_or_else(
            || "unknown model".into(),
            |train| {
                model_for_train(train)
                    .map(|model| model.name().to_owned())
                    .unwrap_or_else(|| format!("unknown model ({})", train.model_id.as_str()))
            },
        );
    PendingAction {
        label: format!("Train resale · Train {:02} ({model})", train_id.get()),
        details: vec!["Sale proceeds were credited only after the save succeeded.".into()],
        funds_before: state.player_company.funds,
    }
}
