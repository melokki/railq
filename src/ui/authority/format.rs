//! Shared formatting and project-label helpers for the Authority workspace.

use ratatui::text::{Line, Span};

use crate::{
    model::{
        ConstructionDifficulty, Electrification, GameState, InfrastructureAccessDiscount,
        InfrastructureProject, InfrastructureProjectKind,
        InfrastructureProjectStatus, Money, UtcSeconds,
    },
    sim::authority::{
        deferred_reconsideration_threshold, local_rail_success_basis_points,
        project_connection_station_id,
    },
    ui::{format as ui_format, theme},
};

pub(super) fn money_line(label: &str, value: Money) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(ui_format::money(value), theme::primary_value()),
    ])
}

pub(super) fn project_scope(state: &GameState, project: &InfrastructureProject) -> String {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            if let Some(route) = new_line_route_label(state, planned_stations, planned_lines) {
                format!("New line · {route}")
            } else {
                let places = planned_stations
                    .iter()
                    .map(|station| settlement_name(state, station.settlement_id))
                    .collect::<Vec<_>>();
                if places.is_empty() {
                    "New line".into()
                } else {
                    format!("New line · {}", places.join(" / "))
                }
            }
        }
        InfrastructureProjectKind::SpeedUpgrade { rail_line_ids, .. } => {
            format!("Speed upgrade · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::DoubleTracking { rail_line_ids, .. } => {
            format!("Double tracking · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::Electrification { rail_line_ids } => {
            format!("Electrification · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::Renewal { rail_line_ids } => {
            format!("Renewal · {} segment(s)", rail_line_ids.len())
        }
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => {
            format!("Station upgrade · {} station(s)", rail_station_ids.len())
        }
    }
}

pub(super) fn new_line_route_label(
    state: &GameState,
    planned_stations: &[crate::model::PlannedRailStation],
    planned_lines: &[crate::model::PlannedRailLine],
) -> Option<String> {
    let first_line = planned_lines.first()?;
    let is_planned_station = |station_id| {
        planned_stations
            .iter()
            .any(|station| station.id == station_id)
    };

    // New connection projects currently grow outward from the existing network.
    // Prefer an existing endpoint as the route origin so the UI reads naturally
    // as "Existing station → New settlement" regardless of stored endpoint order.
    let origin_id = planned_lines
        .iter()
        .flat_map(|line| [line.first_station_id, line.second_station_id])
        .find(|station_id| !is_planned_station(*station_id))
        .unwrap_or(first_line.first_station_id);

    // Prefer a planned endpoint at the edge of the planned graph. This also
    // produces a useful origin → destination label if a later NewLine project
    // contains more than one planned segment/station.
    let destination_id = planned_stations
        .iter()
        .map(|station| station.id)
        .find(|station_id| {
            planned_lines
                .iter()
                .filter(|line| {
                    line.first_station_id == *station_id || line.second_station_id == *station_id
                })
                .count()
                == 1
        })
        .or_else(|| planned_stations.last().map(|station| station.id))
        .unwrap_or(first_line.second_station_id);

    let origin = station_name(state, planned_stations, origin_id);
    let destination = station_name(state, planned_stations, destination_id);
    Some(format!("{origin} → {destination}"))
}

pub(super) fn station_name(
    state: &GameState,
    planned_stations: &[crate::model::PlannedRailStation],
    station_id: crate::model::RailStationId,
) -> String {
    if let Some(station) = planned_stations
        .iter()
        .find(|station| station.id == station_id)
    {
        return settlement_name(state, station.settlement_id);
    }

    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
        .map(|station| settlement_name(state, station.settlement_id))
        .unwrap_or_else(|| "Unknown station".into())
}

pub(super) fn settlement_name(state: &GameState, settlement_id: crate::model::SettlementId) -> String {
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| "Unknown settlement".into())
}

pub(super) fn project_status(status: InfrastructureProjectStatus) -> &'static str {
    match status {
        InfrastructureProjectStatus::Requested => "REQUESTED",
        InfrastructureProjectStatus::UnderReview => "UNDER REVIEW",
        InfrastructureProjectStatus::Proposed => "PROPOSED",
        InfrastructureProjectStatus::Approved => "APPROVED",
        InfrastructureProjectStatus::Deferred => "DEFERRED",
        InfrastructureProjectStatus::Rejected => "REJECTED",
        InfrastructureProjectStatus::Funding => "FUNDING",
        InfrastructureProjectStatus::Scheduled => "SCHEDULED",
        InfrastructureProjectStatus::Construction => "CONSTRUCTION",
        InfrastructureProjectStatus::Open => "OPEN",
        InfrastructureProjectStatus::Cancelled => "CANCELLED",
    }
}

pub(super) fn status_style(status: InfrastructureProjectStatus) -> ratatui::style::Style {
    match status {
        InfrastructureProjectStatus::Open => theme::success(),
        InfrastructureProjectStatus::Deferred => theme::warning(),
        InfrastructureProjectStatus::Rejected | InfrastructureProjectStatus::Cancelled => {
            theme::error()
        }
        InfrastructureProjectStatus::Funding
        | InfrastructureProjectStatus::Scheduled
        | InfrastructureProjectStatus::Construction => theme::warning(),
        _ => theme::primary_value(),
    }
}

pub(super) fn project_next(state: &GameState, project: &InfrastructureProject, now: UtcSeconds) -> String {
    match project.status {
        InfrastructureProjectStatus::Requested => planning_stage_next(
            "Review",
            project.timeline.requested_at,
            state.rules.authority.request_queue_delay(),
            now,
        ),
        InfrastructureProjectStatus::UnderReview => project
            .timeline
            .review_started_at
            .map(|started_at| {
                planning_stage_next(
                    "Proposal",
                    started_at,
                    state.rules.authority.review_duration(),
                    now,
                )
            })
            .unwrap_or_else(|| "Authority review in progress".into()),
        InfrastructureProjectStatus::Proposed => project
            .timeline
            .proposed_at
            .map(|proposed_at| {
                planning_stage_next(
                    "Decision",
                    proposed_at,
                    state.rules.authority.proposal_duration(),
                    now,
                )
            })
            .unwrap_or_else(|| "Authority decision pending".into()),
        InfrastructureProjectStatus::Approved => "Awaiting funding slot".into(),
        InfrastructureProjectStatus::Deferred => deferred_next(state, project, now),
        InfrastructureProjectStatus::Rejected => "Rejected · no automatic review".into(),
        InfrastructureProjectStatus::Funding => project
            .funding
            .funding_gap()
            .map(|gap| {
                if gap <= Money::ZERO {
                    "Scheduling pending".into()
                } else {
                    format!("Still needs {}", ui_format::money(gap))
                }
            })
            .unwrap_or_else(|_| "Funding pending".into()),
        InfrastructureProjectStatus::Scheduled => project
            .timeline
            .scheduled_start_at
            .map(|value| format!("Starts {}", relative_time(value, now)))
            .unwrap_or_else(|| "Construction slot pending".into()),
        InfrastructureProjectStatus::Construction => project
            .timeline
            .planned_completion_at
            .map(|value| format!("Opens {}", relative_time(value, now)))
            .unwrap_or_else(|| "Opening pending".into()),
        InfrastructureProjectStatus::Open => "Complete".into(),
        InfrastructureProjectStatus::Cancelled => "Cancelled".into(),
    }
}

pub(super) fn planning_stage_next(
    label: &str,
    started_at: UtcSeconds,
    delay: crate::model::DurationSeconds,
    now: UtcSeconds,
) -> String {
    let Ok(due_at) = started_at.checked_add(delay) else {
        return format!("{label} pending");
    };
    if due_at <= now {
        format!("{label} ready")
    } else {
        format!("{label} {}", relative_time(due_at, now))
    }
}

pub(super) fn deferred_next(state: &GameState, project: &InfrastructureProject, now: UtcSeconds) -> String {
    let required = deferred_reconsideration_threshold(project.timeline.reconsideration_count);
    if required > 10_000 {
        return "No further automatic review".into();
    }

    let current_maturity = project_connection_station_id(project)
        .map(|station_id| {
            local_rail_success_basis_points(&state.origin_destination_demand, station_id)
        })
        .unwrap_or(0);
    let maturity_ready = current_maturity >= required;
    let eligible_at = project.timeline.deferred_at.and_then(|deferred_at| {
        deferred_at
            .checked_add(state.rules.authority.deferred_reconsideration_delay())
            .ok()
    });
    let cooldown_ready = eligible_at.map_or(true, |eligible_at| eligible_at <= now);

    match (cooldown_ready, maturity_ready, eligible_at) {
        (true, true, _) => "Reconsideration ready".into(),
        (true, false, _) => format!("Needs {} adoption", maturity_percent(required)),
        (false, true, Some(eligible_at)) => format!("Eligible {}", relative_time(eligible_at, now)),
        (false, false, Some(eligible_at)) => format!(
            "Eligible {} · needs {} adoption",
            relative_time(eligible_at, now),
            maturity_percent(required)
        ),
        _ => format!("Needs {} adoption", maturity_percent(required)),
    }
}

pub(super) fn maturity_percent(basis_points: u16) -> String {
    format!("{}%", u32::from(basis_points).saturating_add(50) / 100)
}

pub(super) fn maturity_label(basis_points: u16) -> &'static str {
    match basis_points {
        0..=3_499 => "Emerging",
        3_500..=5_999 => "Growing",
        6_000..=8_499 => "Established",
        _ => "Mature",
    }
}

pub(crate) fn access_discount_label(
    discount: InfrastructureAccessDiscount,
    now: UtcSeconds,
) -> String {
    let percent = u32::from(discount.basis_points) / 100;
    if !discount.is_active_at(now) {
        return format!("{percent}% · expired");
    }

    let remaining = discount
        .expires_at
        .unix_seconds()
        .saturating_sub(now.unix_seconds())
        .try_into()
        .unwrap_or(0);
    format!("{percent}% · {} remaining", compact_duration(remaining))
}

pub(super) fn format_project_timestamp(timestamp: UtcSeconds, now: UtcSeconds) -> String {
    let (year, month, day, hour, minute) = utc_date_time(timestamp);
    let (now_year, now_month, now_day, _, _) = utc_date_time(now);
    let timestamp_day = timestamp.unix_seconds().div_euclid(86_400);
    let now_day_index = now.unix_seconds().div_euclid(86_400);

    if (year, month, day) == (now_year, now_month, now_day) {
        format!("Today {hour:02}:{minute:02} UTC")
    } else if timestamp_day == now_day_index.saturating_add(1) {
        format!("Tomorrow {hour:02}:{minute:02} UTC")
    } else {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
    }
}

pub(super) fn utc_date_time(timestamp: UtcSeconds) -> (i64, i64, i64, i64, i64) {
    let unix = timestamp.unix_seconds();
    let days = unix.div_euclid(86_400);
    let seconds_of_day = unix.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_unix_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    (year, month, day, hour, minute)
}

pub(super) fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    let shifted = days.saturating_add(719_468);
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(super) fn relative_time(timestamp: UtcSeconds, now: UtcSeconds) -> String {
    let delta = timestamp.unix_seconds().saturating_sub(now.unix_seconds());
    if delta == 0 {
        return "now".into();
    }
    let seconds = delta.unsigned_abs();
    if delta > 0 {
        format!("in {}", compact_duration(seconds))
    } else {
        format!("{} ago", compact_duration(seconds))
    }
}

pub(super) fn construction_remaining_duration(seconds: u64) -> String {
    if seconds <= 30 * 60 {
        let minutes = seconds / 60;
        let seconds = seconds % 60;
        if minutes == 0 {
            return format!("{seconds}s");
        }
        return format!("{minutes}m {seconds:02}s");
    }

    compact_duration(seconds)
}

pub(super) fn compact_duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let total_minutes = seconds / 60;
    if total_minutes < 60 {
        return format!("{total_minutes}m");
    }

    let total_hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if total_hours < 24 {
        if minutes == 0 {
            return format!("{total_hours}h");
        }
        return format!("{total_hours}h {minutes}m");
    }

    let days = total_hours / 24;
    let hours = total_hours % 24;
    if hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {hours}h")
    }
}

pub(super) fn electrification_label(value: Electrification) -> &'static str {
    match value {
        Electrification::None => "non-electrified",
        Electrification::Electric => "electric",
    }
}

pub(super) fn difficulty_label(value: ConstructionDifficulty) -> &'static str {
    match value {
        ConstructionDifficulty::Low => "low",
        ConstructionDifficulty::Moderate => "moderate",
        ConstructionDifficulty::High => "high",
    }
}
