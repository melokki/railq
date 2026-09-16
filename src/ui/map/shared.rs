//! Shared presentation helpers used by Map surfaces.

use crate::{
    catalog::model_for_train,
    model::{GameState, Journey, Money, RailStationId, Train, UtcSeconds},
};

pub(super) fn train_model_name(train: &Train) -> String {
    let model = model_for_train(train)
        .map(|model| model.name().to_owned())
        .unwrap_or_else(|| format!("Unknown model ({})", train.model_id.as_str()));
    train
        .nickname
        .as_ref()
        .map(|nickname| format!("{} · {model}", nickname.as_str()))
        .unwrap_or(model)
}

pub(super) fn station_label(state: &GameState, station_id: RailStationId) -> &str {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return "unknown Rail Station";
    };
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map_or("unknown Settlement", |settlement| settlement.name.as_str())
}

pub(super) fn journey_progress_percent(journey: &Journey, now: UtcSeconds) -> u64 {
    let duration = journey
        .arrives_at
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds());
    if duration <= 0 {
        return 100;
    }
    let elapsed = now
        .unix_seconds()
        .saturating_sub(journey.departed_at.unix_seconds())
        .clamp(0, duration);
    u64::try_from(elapsed.saturating_mul(100) / duration).unwrap_or(100)
}

pub(super) fn remaining_seconds(journey: &Journey, now: UtcSeconds) -> u64 {
    u64::try_from(
        journey
            .arrives_at
            .unix_seconds()
            .saturating_sub(now.unix_seconds())
            .max(0),
    )
    .unwrap_or(u64::MAX)
}

pub(super) fn format_money(money: Money) -> String {
    crate::ui::format::money(money)
}

pub(super) fn format_distance(metres: u64) -> String {
    crate::ui::format::distance(metres)
}

pub(super) fn format_duration(seconds: u64) -> String {
    crate::ui::format::duration(seconds)
}
