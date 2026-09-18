//! Development-programme presentation for the Rail Authority workspace.
//!
//! This module owns the player-facing project ordering, lifecycle pipeline,
//! and stage-aware project table. Simulation statuses remain unchanged.

use std::cmp::Ordering;

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::{Line, Span, Text},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table},
};

use crate::{
    model::{GameState, InfrastructureProject, InfrastructureProjectStatus, UtcSeconds},
    ui::{components::panel_block, format as ui_format, theme},
};

use super::{
    ProjectSelection,
    analytics::{ProgrammeCounts, ProgrammeStage},
    format::{project_next, project_scope},
};

/// Returns project indices in player-facing programme order.
///
/// Active delivery work appears before planning and history. Within delivery
/// stages, the next scheduled milestone wins; completed/closed work is newest
/// first so old history naturally falls toward the bottom of the table.
pub(super) fn ordered_project_indices(state: &GameState) -> Vec<usize> {
    let projects = &state.region.rail_authority.infrastructure_projects;
    let mut indices = (0..projects.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| compare_projects(&projects[*left], &projects[*right]));
    indices
}

fn compare_projects(left: &InfrastructureProject, right: &InfrastructureProject) -> Ordering {
    let left_stage = ProgrammeStage::from_status(left.status);
    let right_stage = ProgrammeStage::from_status(right.status);

    stage_rank(left_stage)
        .cmp(&stage_rank(right_stage))
        .then_with(|| compare_within_stage(left, right, left_stage))
        .then_with(|| left.timeline.requested_at.cmp(&right.timeline.requested_at))
}

const fn stage_rank(stage: ProgrammeStage) -> u8 {
    match stage {
        ProgrammeStage::Building => 0,
        ProgrammeStage::Queued => 1,
        ProgrammeStage::Funding => 2,
        ProgrammeStage::Planning => 3,
        ProgrammeStage::Deferred => 4,
        ProgrammeStage::Open => 5,
        ProgrammeStage::Closed => 6,
    }
}

fn compare_within_stage(
    left: &InfrastructureProject,
    right: &InfrastructureProject,
    stage: ProgrammeStage,
) -> Ordering {
    match stage {
        ProgrammeStage::Building => left
            .timeline
            .planned_completion_at
            .cmp(&right.timeline.planned_completion_at),
        ProgrammeStage::Queued => left
            .timeline
            .scheduled_start_at
            .cmp(&right.timeline.scheduled_start_at),
        ProgrammeStage::Open => right
            .timeline
            .completed_at
            .cmp(&left.timeline.completed_at),
        ProgrammeStage::Closed => closed_at(right).cmp(&closed_at(left)),
        ProgrammeStage::Planning | ProgrammeStage::Funding | ProgrammeStage::Deferred => {
            left.timeline.requested_at.cmp(&right.timeline.requested_at)
        }
    }
}

fn closed_at(project: &InfrastructureProject) -> Option<UtcSeconds> {
    project
        .timeline
        .cancelled_at
        .or(project.timeline.deferred_at)
        .or(project.timeline.completed_at)
}

pub(super) fn render_programme_pipeline(
    frame: &mut Frame,
    area: Rect,
    counts: ProgrammeCounts,
) {
    let mut lifecycle = vec![
        stage_count_span("PLANNING", counts.planning, ProgrammeStage::Planning),
        separator_span(),
        stage_count_span("FUNDING", counts.funding, ProgrammeStage::Funding),
        separator_span(),
        stage_count_span("QUEUED", counts.queued, ProgrammeStage::Queued),
        separator_span(),
        stage_count_span("BUILDING", counts.building, ProgrammeStage::Building),
        separator_span(),
        stage_count_span("OPEN", counts.open, ProgrammeStage::Open),
    ];
    if counts.deferred > 0 {
        lifecycle.push(Span::styled("   ·   ", theme::secondary()));
        lifecycle.push(stage_count_span(
            "HOLD",
            counts.deferred,
            ProgrammeStage::Deferred,
        ));
    }

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("DEVELOPMENT PIPELINE", theme::table_header()),
            Line::from(lifecycle),
        ])
        .style(theme::panel()),
        area,
    );
}

fn stage_count_span(label: &'static str, count: usize, stage: ProgrammeStage) -> Span<'static> {
    Span::styled(format!("{label} {count}"), stage_style(stage).bold())
}

fn separator_span() -> Span<'static> {
    Span::styled("  →  ", theme::secondary())
}

pub(super) fn render_projects(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
    compact: bool,
) {
    let projects = &state.region.rail_authority.infrastructure_projects;
    if projects.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("No infrastructure projects yet.", theme::secondary()),
                Line::styled(
                    "Local councils will request connections as nearby rail adoption grows.",
                    theme::secondary(),
                ),
            ])
            .block(panel_block("Development Programme", false))
            .style(theme::panel()),
            area,
        );
        return;
    }

    let inner_height = area.height.saturating_sub(3);
    selection.set_page_size(usize::from(inner_height).max(1));
    selection.synchronize(state);
    let order = ordered_project_indices(state);

    let first_history_row = order.iter().position(|source_index| {
        matches!(
            ProgrammeStage::from_status(projects[*source_index].status),
            ProgrammeStage::Open | ProgrammeStage::Closed
        )
    });
    let separator_after = first_history_row.and_then(|position| position.checked_sub(1));
    if separator_after.is_some() {
        selection.set_page_size(usize::from(inner_height.saturating_sub(1)).max(1));
    }

    let rows = order
        .iter()
        .enumerate()
        .map(|(display_index, source_index)| {
            let project = &projects[*source_index];
            let stage = ProgrammeStage::from_status(project.status);
            let row = if compact {
                Row::new(vec![
                    Cell::from(format!("{:02}", source_index + 1)),
                    Cell::from(project_scope(state, project)),
                    Cell::from(stage_label(stage)).style(stage_style(stage)),
                    Cell::from(stage_progress(project, now)),
                ])
            } else {
                Row::new(vec![
                    Cell::from(format!("{:02}", source_index + 1)),
                    Cell::from(stage_label(stage)).style(stage_style(stage)),
                    Cell::from(project_scope(state, project)),
                    Cell::from(
                        Text::from(ui_format::money(project.funding.estimated_cost)).right_aligned(),
                    ),
                    Cell::from(stage_progress(project, now)),
                    Cell::from(programme_next(state, project, now)),
                ])
            };

            if separator_after == Some(display_index) {
                row.bottom_margin(1)
            } else {
                row
            }
        })
        .collect::<Vec<_>>();

    let (header, widths) = if compact {
        (
            Row::new(["#", "Project", "Stage", "Progress"]).style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(10),
                Constraint::Length(16),
            ],
        )
    } else {
        (
            Row::new(vec![
                Cell::from("#"),
                Cell::from("Stage"),
                Cell::from("Project"),
                Cell::from(Text::from("Budget").right_aligned()),
                Cell::from("Progress"),
                Cell::from("Next"),
            ])
            .style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Length(10),
                Constraint::Fill(2),
                Constraint::Length(13),
                Constraint::Length(16),
                Constraint::Fill(1),
            ],
        )
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(panel_block("Programme", false))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

fn programme_next(state: &GameState, project: &InfrastructureProject, now: UtcSeconds) -> String {
    match project.status {
        InfrastructureProjectStatus::Open
        | InfrastructureProjectStatus::Rejected
        | InfrastructureProjectStatus::Cancelled => "—".into(),
        _ => project_next(state, project, now),
    }
}

pub(super) fn stage_label(stage: ProgrammeStage) -> &'static str {
    match stage {
        ProgrammeStage::Planning => "PLANNING",
        ProgrammeStage::Funding => "FUNDING",
        ProgrammeStage::Queued => "QUEUED",
        ProgrammeStage::Building => "BUILDING",
        ProgrammeStage::Open => "OPEN",
        ProgrammeStage::Deferred => "HOLD",
        ProgrammeStage::Closed => "CLOSED",
    }
}

pub(super) fn stage_style(stage: ProgrammeStage) -> ratatui::style::Style {
    match stage {
        ProgrammeStage::Open => theme::success(),
        ProgrammeStage::Funding
        | ProgrammeStage::Queued
        | ProgrammeStage::Building
        | ProgrammeStage::Deferred => theme::warning(),
        ProgrammeStage::Closed => theme::error(),
        ProgrammeStage::Planning => theme::primary_value(),
    }
}

fn stage_progress(project: &InfrastructureProject, now: UtcSeconds) -> String {
    match project.status {
        InfrastructureProjectStatus::Requested => "Awaiting review".into(),
        InfrastructureProjectStatus::UnderReview => "In review".into(),
        InfrastructureProjectStatus::Proposed => "Proposal".into(),
        InfrastructureProjectStatus::Approved => "Approved".into(),
        InfrastructureProjectStatus::Deferred => "On hold".into(),
        InfrastructureProjectStatus::Rejected => "Rejected".into(),
        InfrastructureProjectStatus::Funding => funding_progress(project),
        InfrastructureProjectStatus::Scheduled => "Ready to build".into(),
        InfrastructureProjectStatus::Construction => construction_progress(project, now),
        InfrastructureProjectStatus::Open => "Complete".into(),
        InfrastructureProjectStatus::Cancelled => "Cancelled".into(),
    }
}

fn funding_progress(project: &InfrastructureProject) -> String {
    let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
    if estimated == 0 {
        return "Funding pending".into();
    }
    let funded = project
        .funding
        .total_funded()
        .map(|money| i128::from(money.cents()).max(0))
        .unwrap_or(0);
    let percent = funded
        .saturating_mul(100)
        .saturating_div(estimated)
        .min(100) as u8;
    progress_bar(percent)
}

fn construction_progress(project: &InfrastructureProject, now: UtcSeconds) -> String {
    let Some(started_at) = project.timeline.construction_started_at else {
        return "Mobilising".into();
    };
    let Some(completion_at) = project.timeline.planned_completion_at else {
        return "In progress".into();
    };

    let duration = completion_at
        .unix_seconds()
        .saturating_sub(started_at.unix_seconds())
        .max(0) as u64;
    if duration == 0 {
        return progress_bar(100);
    }
    let elapsed = now
        .unix_seconds()
        .saturating_sub(started_at.unix_seconds())
        .max(0) as u64;
    let percent = elapsed
        .min(duration)
        .saturating_mul(100)
        .saturating_div(duration) as u8;
    progress_bar(percent)
}

fn progress_bar(percent: u8) -> String {
    const WIDTH: usize = 6;
    let filled = usize::from(percent).saturating_mul(WIDTH) / 100;
    format!(
        "{}{} {:>3}%",
        "█".repeat(filled),
        "░".repeat(WIDTH.saturating_sub(filled)),
        percent
    )
}
