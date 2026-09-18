//! Stage-specific selected-project presentation for the Authority workspace.
//!
//! The programme table answers "what is happening?". This surface answers
//! "what matters about the selected project right now?" by changing emphasis
//! with the project's lifecycle stage instead of rendering one long dossier.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    model::{
        GameState, InfrastructureProject, InfrastructureProjectKind, InfrastructureProjectStatus,
        Money, PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS,
        PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS, UtcSeconds,
    },
    sim::authority::{
        AUTHORITY_APPROVAL_SCORE_THRESHOLD, AUTHORITY_REJECTION_SCORE_THRESHOLD,
        local_rail_success_basis_points, project_connection_station_id,
        project_review_score_breakdown,
    },
    ui::{
        components::{labelled_line, labelled_line_styled, panel_block, section_heading},
        format as ui_format, theme,
    },
};

use super::{
    ProjectSelection,
    analytics::ProgrammeStage,
    format::{
        access_discount_label, construction_remaining_duration, deferred_next, difficulty_label,
        electrification_label, format_project_timestamp, maturity_label, maturity_percent,
        new_line_route_label, project_next, project_scope, project_status, relative_time,
        settlement_name, status_style,
    },
    programme::{stage_label, stage_style},
};


pub(super) fn preferred_project_detail_height(
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) -> u16 {
    let Some((_, project)) = selection.selected_project(state) else {
        return 8;
    };

    let content_lines = project_context_lines(state, project)
        .len()
        .max(stage_focus_lines(state, now, project).len());
    let content_height = u16::try_from(content_lines).unwrap_or(u16::MAX);

    // Border (2) + header (2) + two gaps (2) + milestones (1).
    content_height.saturating_add(7).clamp(14, 20)
}

pub(super) fn render_selected_project(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let Some((index, project)) = selection.selected_project(state) else {
        frame.render_widget(
            Paragraph::new("No project selected.")
                .block(panel_block("Selected Project", false))
                .style(theme::panel()),
            area,
        );
        return;
    };

    let block = panel_block("Selected Project", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if inner.height < 10 {
        render_project_summary(frame, inner, state, now, index, project);
        return;
    }

    if inner.height < 14 {
        let [header_area, stage_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Fill(1)])
                .spacing(1)
                .areas(inner);
        render_project_header(frame, header_area, state, now, index, project);
        render_stage_focus(frame, stage_area, state, now, project);
        return;
    }

    let [header_area, content_area, milestones_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .spacing(1)
    .areas(inner);

    render_project_header(frame, header_area, state, now, index, project);

    if content_area.width >= 82 {
        let [context_area, stage_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(2)
                .areas(content_area);
        render_project_context(frame, context_area, state, project);
        render_stage_focus(frame, stage_area, state, now, project);
    } else {
        let [stage_area, context_area] =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                .spacing(1)
                .areas(content_area);
        render_stage_focus(frame, stage_area, state, now, project);
        render_project_context(frame, context_area, state, project);
    }

    render_milestones(frame, milestones_area, now, project);
}


fn render_project_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    index: usize,
    project: &InfrastructureProject,
) {
    let mut lines = vec![
        Line::styled(
            format!("PROJECT {:02} · {}", index + 1, project_heading(state, project)),
            theme::title(),
        ),
        Line::from({
            let stage = ProgrammeStage::from_status(project.status);
            vec![
                Span::styled(stage_label(stage), stage_style(stage).bold()),
                Span::styled(" · ", theme::secondary()),
                Span::styled(project_next(state, project, now), theme::primary_value()),
            ]
        }),
    ];
    if area.height >= 3 {
        lines.push(Line::from(vec![
            Span::styled("Budget  ", theme::secondary()),
            Span::styled(
                ui_format::money(project.funding.estimated_cost),
                theme::primary_value(),
            ),
            Span::styled(" · Operator  ", theme::secondary()),
            Span::styled(
                ui_format::money(project.funding.operator_contributed),
                if project.funding.operator_contributed > Money::ZERO {
                    theme::success()
                } else {
                    theme::primary_value()
                },
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_project_header(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    index: usize,
    project: &InfrastructureProject,
) {
    let heading = project_heading(state, project);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!("PROJECT {:02} · {heading}", index + 1),
                theme::title(),
            ),
            Line::from({
                let stage = ProgrammeStage::from_status(project.status);
                vec![
                    Span::styled(stage_label(stage), stage_style(stage).bold()),
                    Span::styled(" · ", theme::secondary()),
                    Span::styled(project_next(state, project, now), theme::primary_value()),
                ]
            }),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn project_heading(state: &GameState, project: &InfrastructureProject) -> String {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => new_line_route_label(state, planned_stations, planned_lines)
            .unwrap_or_else(|| project_scope(state, project)),
        _ => project_scope(state, project),
    }
}

fn render_project_context(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    project: &InfrastructureProject,
) {
    frame.render_widget(
        Paragraph::new(project_context_lines(state, project))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn project_context_lines(
    state: &GameState,
    project: &InfrastructureProject,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    append_development_case(&mut lines, state, project);
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    append_infrastructure_scope(&mut lines, state, project);
    lines
}

fn append_development_case(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
) {
    let InfrastructureProjectKind::NewLine {
        planned_stations, ..
    } = &project.kind
    else {
        return;
    };
    let Some(planned_station) = planned_stations.first() else {
        return;
    };

    lines.push(section_heading("DEVELOPMENT CASE"));
    lines.push(labelled_line(
        "Requested by",
        &format!(
            "{} Council",
            settlement_name(state, planned_station.settlement_id)
        ),
    ));

    if let Some(connection_station_id) = project_connection_station_id(project) {
        let maturity = local_rail_success_basis_points(
            &state.origin_destination_demand,
            connection_station_id,
        );
        lines.push(labelled_line(
            "Rail adoption",
            &format!("{} · {}", maturity_percent(maturity), maturity_label(maturity)),
        ));
    }
}

fn append_infrastructure_scope(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
) {
    lines.push(section_heading(match project.status {
        InfrastructureProjectStatus::Open => "DELIVERED INFRASTRUCTURE",
        _ => "INFRASTRUCTURE",
    }));

    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            if let Some(route) = new_line_route_label(state, planned_stations, planned_lines) {
                lines.push(labelled_line("Route", &route));
            }
            let total_metres = planned_lines
                .iter()
                .map(|line| line.distance.metres())
                .sum::<u64>();
            lines.push(labelled_line(
                "New station",
                &planned_stations
                    .iter()
                    .map(|station| settlement_name(state, station.settlement_id))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
            lines.push(labelled_line(
                "New track",
                &ui_format::distance(total_metres),
            ));
            if let Some(line) = planned_lines.first() {
                lines.push(labelled_line(
                    "Capability",
                    &format!(
                        "{} km/h · {} track{} · {} · {}",
                        line.speed_limit.kilometres_per_hour(),
                        line.track_count.tracks(),
                        if line.track_count.tracks() == 1 { "" } else { "s" },
                        electrification_label(line.electrification),
                        difficulty_label(line.construction_difficulty),
                    ),
                ));
            }
        }
        InfrastructureProjectKind::SpeedUpgrade {
            rail_line_ids,
            target_speed_limit,
        } => lines.push(labelled_line(
            "Scope",
            &format!(
                "{} segment(s) → {} km/h",
                rail_line_ids.len(),
                target_speed_limit.kilometres_per_hour()
            ),
        )),
        InfrastructureProjectKind::DoubleTracking {
            rail_line_ids,
            target_track_count,
        } => lines.push(labelled_line(
            "Scope",
            &format!(
                "{} segment(s) → {} tracks",
                rail_line_ids.len(),
                target_track_count.tracks()
            ),
        )),
        InfrastructureProjectKind::Electrification { rail_line_ids } => lines.push(labelled_line(
            "Scope",
            &format!("Electrify {} segment(s)", rail_line_ids.len()),
        )),
        InfrastructureProjectKind::Renewal { rail_line_ids } => lines.push(labelled_line(
            "Scope",
            &format!("Renew {} segment(s)", rail_line_ids.len()),
        )),
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => lines.push(labelled_line(
            "Scope",
            &format!("Upgrade {} station(s)", rail_station_ids.len()),
        )),
    }
}

fn render_stage_focus(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    frame.render_widget(
        Paragraph::new(stage_focus_lines(state, now, project))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn stage_focus_lines(
    state: &GameState,
    now: UtcSeconds,
    project: &InfrastructureProject,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match project.status {
        InfrastructureProjectStatus::Requested
        | InfrastructureProjectStatus::UnderReview
        | InfrastructureProjectStatus::Proposed
        | InfrastructureProjectStatus::Approved
        | InfrastructureProjectStatus::Deferred
        | InfrastructureProjectStatus::Rejected => {
            append_decision_focus(&mut lines, state, now, project);
        }
        InfrastructureProjectStatus::Funding => {
            append_funding_focus(&mut lines, now, project);
        }
        InfrastructureProjectStatus::Scheduled => {
            append_scheduled_focus(&mut lines, now, project);
        }
        InfrastructureProjectStatus::Construction => {
            append_construction_focus(&mut lines, now, project);
        }
        InfrastructureProjectStatus::Open => {
            append_open_focus(&mut lines, now, project);
        }
        InfrastructureProjectStatus::Cancelled => {
            append_cancelled_focus(&mut lines, now, project);
        }
    }
    lines
}

fn append_decision_focus(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading(match project.status {
        InfrastructureProjectStatus::Deferred => "AUTHORITY HOLD",
        InfrastructureProjectStatus::Rejected => "AUTHORITY DECISION",
        _ => "AUTHORITY REVIEW",
    }));
    lines.push(labelled_line_styled(
        "Status",
        project_status(project.status),
        status_style(project.status),
    ));
    lines.push(labelled_line(
        "Next",
        &project_next(state, project, now),
    ));

    if let Some(score) = project_review_score_breakdown(&state.region, project, state.world_seed) {
        lines.push(labelled_line_styled(
            "Current case",
            &format!("{} / {}", score.total, AUTHORITY_APPROVAL_SCORE_THRESHOLD),
            if score.total >= AUTHORITY_APPROVAL_SCORE_THRESHOLD {
                theme::success()
            } else {
                theme::warning()
            },
        ));
        lines.push(labelled_line(
            "Public value",
            &format!(
                "+{} pop · +{} demand · +{} network",
                score.population, score.latent_demand, score.network_usefulness
            ),
        ));
        lines.push(labelled_line(
            "Regional / cost",
            &format!(
                "+{} development · -{} build",
                score.regional_development, score.construction_cost_penalty
            ),
        ));
        lines.push(labelled_line(
            "Decision bands",
            &format!(
                "approve ≥{} · defer {}–{} · reject <{}",
                AUTHORITY_APPROVAL_SCORE_THRESHOLD,
                AUTHORITY_REJECTION_SCORE_THRESHOLD,
                AUTHORITY_APPROVAL_SCORE_THRESHOLD - 1,
                AUTHORITY_REJECTION_SCORE_THRESHOLD,
            ),
        ));
    }

    if project.status == InfrastructureProjectStatus::Deferred {
        lines.push(labelled_line(
            "Reconsideration",
            &deferred_next(state, project, now),
        ));
    } else if project.status == InfrastructureProjectStatus::Rejected {
        lines.push(labelled_line("Reconsideration", "Not automatic"));
    }

    if project.timeline.reconsideration_count > 0 {
        lines.push(labelled_line(
            "Reconsidered",
            &format!("{} time(s)", project.timeline.reconsideration_count),
        ));
    }
}

fn append_funding_focus(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading("FUNDING"));
    lines.push(progress_line("Funding", funding_percent(project)));
    append_financial_split(lines, project);
    if let Ok(gap) = project.funding.funding_gap() {
        lines.push(labelled_line_styled(
            "Funding gap",
            &ui_format::money(gap),
            if gap <= Money::ZERO {
                theme::success()
            } else {
                theme::warning()
            },
        ));
    }
    append_operator_involvement(lines, now, project);
}

fn append_scheduled_focus(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading("DELIVERY PLAN"));
    if let Some(start) = project.timeline.scheduled_start_at {
        lines.push(labelled_line(
            "Starts",
            &format!(
                "{} · {}",
                format_project_timestamp(start, now),
                relative_time(start, now)
            ),
        ));
    } else {
        lines.push(labelled_line("Starts", "Awaiting construction slot"));
    }
    append_financial_split(lines, project);
    append_operator_involvement(lines, now, project);
}

fn append_construction_focus(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading("DELIVERY & FUNDING"));
    if let Some((percent, remaining)) = construction_progress(project, now) {
        lines.push(progress_line("Construction", percent));
        lines.push(labelled_line(
            "Remaining",
            &construction_remaining_duration(remaining),
        ));
    }
    if let Some(started) = project.timeline.construction_started_at {
        lines.push(labelled_line("Started", &relative_time(started, now)));
    }
    if let Some(completion) = project.timeline.planned_completion_at {
        lines.push(labelled_line_styled(
            "Opens",
            &format_project_timestamp(completion, now),
            theme::success(),
        ));
    }
    append_financial_split(lines, project);
    append_operator_involvement(lines, now, project);
}

fn append_open_focus(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading("DELIVERED"));
    if let Some(opened) = project.timeline.completed_at {
        lines.push(labelled_line_styled(
            "Opened",
            &format!(
                "{} · {}",
                format_project_timestamp(opened, now),
                relative_time(opened, now)
            ),
            theme::success(),
        ));
    }
    append_financial_split(lines, project);
    append_operator_involvement(lines, now, project);
}

fn append_cancelled_focus(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(section_heading("PROJECT OUTCOME"));
    lines.push(labelled_line_styled(
        "Status",
        "CANCELLED",
        theme::error(),
    ));
    if let Some(cancelled) = project.timeline.cancelled_at {
        lines.push(labelled_line(
            "Cancelled",
            &format!(
                "{} · {}",
                format_project_timestamp(cancelled, now),
                relative_time(cancelled, now)
            ),
        ));
    }
    append_financial_split(lines, project);
    append_operator_involvement(lines, now, project);
}

fn append_financial_split(lines: &mut Vec<Line<'static>>, project: &InfrastructureProject) {
    lines.push(labelled_line(
        "Estimated cost",
        &ui_format::money(project.funding.estimated_cost),
    ));
    lines.push(labelled_line(
        "Authority funding",
        &ui_format::money(project.funding.authority_committed),
    ));
}

fn append_operator_involvement(
    lines: &mut Vec<Line<'static>>,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    lines.push(Line::from(""));
    lines.push(section_heading("YOUR INVOLVEMENT"));
    lines.push(labelled_line(
        "Contribution",
        &ui_format::money(project.funding.operator_contributed),
    ));

    let contribution_room = project
        .funding
        .remaining_operator_contribution_capacity()
        .ok()
        .map(|remaining| remaining.max(Money::ZERO));
    if project.status == InfrastructureProjectStatus::Funding {
        if let Some(remaining) = contribution_room {
            if remaining > Money::ZERO {
                lines.push(labelled_line(
                    "Contribution room",
                    &ui_format::money(remaining),
                ));
            } else {
                lines.push(labelled_line_styled(
                    "Contribution limit",
                    "Reached",
                    theme::success(),
                ));
            }
        }
    }

    if let Some(discount) = project.funding.access_fee_discount {
        lines.push(labelled_line_styled(
            "Access benefit",
            &access_discount_label(discount, now),
            if discount.is_active_at(now) {
                theme::success()
            } else {
                theme::secondary()
            },
        ));
    } else if project.funding.operator_contributed > Money::ZERO {
        let percent = u32::from(PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS) / 100;
        lines.push(labelled_line_styled(
            "Access benefit",
            &format!(
                "{percent}% · {} fiscal days after opening",
                PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS
            ),
            theme::success(),
        ));
    } else if project.status == InfrastructureProjectStatus::Funding
        && contribution_room.is_some_and(|remaining| remaining > Money::ZERO)
    {
        let percent = u32::from(PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS) / 100;
        lines.push(labelled_line(
            "Potential benefit",
            &format!(
                "{percent}% · {} fiscal days after opening",
                PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS
            ),
        ));
    } else {
        lines.push(labelled_line("Access fees", "Normal rates apply"));
    }
}

fn funding_percent(project: &InfrastructureProject) -> u8 {
    let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
    let committed = project
        .funding
        .total_funded()
        .map(|money| i128::from(money.cents()).max(0))
        .unwrap_or(0);
    if estimated == 0 {
        return 0;
    }
    committed
        .saturating_mul(100)
        .saturating_div(estimated)
        .min(100) as u8
}

fn construction_progress(project: &InfrastructureProject, now: UtcSeconds) -> Option<(u8, u64)> {
    let started = project.timeline.construction_started_at?;
    let completion = project.timeline.planned_completion_at?;
    let duration = completion
        .unix_seconds()
        .saturating_sub(started.unix_seconds())
        .max(0) as u64;
    let elapsed = now
        .unix_seconds()
        .saturating_sub(started.unix_seconds())
        .max(0) as u64;
    let remaining = completion
        .unix_seconds()
        .saturating_sub(now.unix_seconds())
        .max(0) as u64;
    let progress = if duration == 0 {
        100
    } else {
        elapsed
            .min(duration)
            .saturating_mul(100)
            .saturating_div(duration) as u8
    };
    Some((progress, remaining))
}

fn progress_line(label: &str, percent: u8) -> Line<'static> {
    let percent = percent.min(100);
    let filled = usize::from(percent).saturating_mul(10).saturating_add(50) / 100;
    let empty = 10usize.saturating_sub(filled);
    Line::from(vec![
        Span::styled(format!("{label:<18}"), theme::secondary()),
        Span::styled("█".repeat(filled), theme::warning()),
        Span::styled("░".repeat(empty), theme::secondary()),
        Span::styled(format!("  {percent}%"), theme::primary_value()),
    ])
}

fn render_milestones(
    frame: &mut Frame,
    area: Rect,
    now: UtcSeconds,
    project: &InfrastructureProject,
) {
    let mut milestones = vec![Span::styled("MILESTONES  ", theme::table_header())];
    push_milestone(
        &mut milestones,
        "Requested",
        project.timeline.requested_at,
        now,
    );
    if let Some(value) = project.timeline.approved_at {
        push_milestone(&mut milestones, "Approved", value, now);
    }
    if let Some(value) = project.timeline.funding_completed_at {
        push_milestone(&mut milestones, "Funded", value, now);
    }
    if let Some(value) = project.timeline.construction_started_at {
        push_milestone(&mut milestones, "Build", value, now);
    }
    if let Some(value) = project.timeline.completed_at {
        push_milestone(&mut milestones, "Open", value, now);
    } else if let Some(value) = project.timeline.planned_completion_at {
        push_milestone(&mut milestones, "Open", value, now);
    }

    frame.render_widget(
        Paragraph::new(vec![Line::from(milestones)])
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn push_milestone(
    spans: &mut Vec<Span<'static>>,
    label: &str,
    timestamp: UtcSeconds,
    now: UtcSeconds,
) {
    if spans.len() > 1 {
        spans.push(Span::styled("  →  ", theme::secondary()));
    }
    spans.push(Span::styled(format!("{label} "), theme::secondary()));
    spans.push(Span::styled(relative_time(timestamp, now), theme::primary_value()));
}
