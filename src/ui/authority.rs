//! Rail Authority infrastructure programme presentation.
//!
//! This workspace exposes the public infrastructure budget, construction
//! capacity, persisted project pipeline, and optional Player Company funding
//! contributions without giving the operator control of Authority decisions.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{
        ConstructionDifficulty, Electrification, GameState, InfrastructureProject,
        InfrastructureProjectId, InfrastructureProjectKind, InfrastructureProjectStatus, Money,
        UtcSeconds,
    },
    sim::authority::{
        AUTHORITY_APPROVAL_SCORE_THRESHOLD, AUTHORITY_REJECTION_SCORE_THRESHOLD,
        deferred_reconsideration_threshold, local_rail_success_basis_points,
        project_connection_station_id, project_review_score_breakdown,
    },
    ui::{components::panel_block, format, modal, theme},
};

/// Persistent read-only project focus for the Authority workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectSelection {
    selected_project_id: Option<InfrastructureProjectId>,
    table_state: TableState,
    page_size: usize,
}

impl ProjectSelection {
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let projects = &state.region.rail_authority.infrastructure_projects;
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k' | 'K') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j' | 'J') => selected
                .saturating_add(1)
                .min(projects.len().saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(projects.len().saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    fn synchronize(&mut self, state: &GameState) {
        let projects = &state.region.rail_authority.infrastructure_projects;
        let previous_index = self.table_state.selected();
        let selected = self
            .selected_project_id
            .and_then(|project_id| projects.iter().position(|project| project.id == project_id))
            .or_else(|| previous_index.map(|index| index.min(projects.len().saturating_sub(1))))
            .or_else(|| {
                projects.iter().position(|project| {
                    !matches!(
                        project.status,
                        InfrastructureProjectStatus::Open
                            | InfrastructureProjectStatus::Rejected
                            | InfrastructureProjectStatus::Cancelled
                    )
                })
            })
            .or_else(|| (!projects.is_empty()).then_some(projects.len().saturating_sub(1)));
        if let Some(index) = selected {
            self.selected_project_id = Some(projects[index].id);
        } else {
            self.selected_project_id = None;
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(project) = state
            .region
            .rail_authority
            .infrastructure_projects
            .get(index)
        else {
            return;
        };
        self.selected_project_id = Some(project.id);
        self.table_state.select(Some(index));
    }

    fn selected_project<'a>(
        &mut self,
        state: &'a GameState,
    ) -> Option<(usize, &'a InfrastructureProject)> {
        self.synchronize(state);
        let index = self.table_state.selected()?;
        state
            .region
            .rail_authority
            .infrastructure_projects
            .get(index)
            .map(|project| (index, project))
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }

    pub fn selected_project_id(&mut self, state: &GameState) -> Option<InfrastructureProjectId> {
        self.selected_project(state).map(|(_, project)| project.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContributionReview {
    pub project_id: InfrastructureProjectId,
    pub amount: Money,
}

/// Shell-facing outcome from the Rail Authority workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityWorkspaceAction {
    /// No application action is required.
    Continue,
    /// Clear stale Shell feedback after an Authority-only transition.
    ClearNotice,
    /// Surface presentation feedback without crossing the application boundary.
    Notice(String),
    /// Revalidate and persist the proposed infrastructure contribution.
    Contribute {
        project_id: InfrastructureProjectId,
        amount: Money,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityShortcut {
    pub key: String,
    pub action: String,
    pub enabled: bool,
}

impl AuthorityShortcut {
    fn enabled(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            enabled: true,
        }
    }

    fn disabled(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            enabled: false,
        }
    }
}

/// Owns presentation state and keyboard interaction for the Rail Authority workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthorityWorkspace {
    selection: ProjectSelection,
    contribution_review: Option<ContributionReview>,
}

impl AuthorityWorkspace {
    /// Returns whether the contribution review currently owns the focused modal layer.
    pub fn has_modal(&self) -> bool {
        self.contribution_review.is_some()
    }

    /// Returns whether the focused project currently accepts an operator contribution.
    pub fn can_contribute(&mut self, state: &GameState) -> bool {
        self.selection
            .selected_project_id(state)
            .and_then(|project_id| ContributionReview::start(state, project_id).ok())
            .is_some()
    }

    /// Returns the contextual footer actions for the current Authority step.
    pub fn shortcuts(
        &mut self,
        state: &GameState,
        compact: bool,
        wide: bool,
    ) -> Vec<AuthorityShortcut> {
        if self.has_modal() {
            return vec![
                AuthorityShortcut::enabled("Enter", "Contribute"),
                AuthorityShortcut::enabled("Esc", "Cancel"),
            ];
        }

        if state
            .region
            .rail_authority
            .infrastructure_projects
            .is_empty()
        {
            return vec![AuthorityShortcut::disabled("↑↓", "Project")];
        }

        let mut items = vec![AuthorityShortcut::enabled(
            if compact { "↑↓" } else { "↑↓/JK" },
            "Project",
        )];
        if wide {
            items.push(AuthorityShortcut::enabled("PgUp/PgDn", "Page"));
        }
        items.push(if self.can_contribute(state) {
            AuthorityShortcut::enabled("F", "Contribute")
        } else {
            AuthorityShortcut::disabled("F", "Contribute")
        });
        items
    }

    /// Returns help content for the currently focused Authority state.
    pub fn help_lines(&self) -> Vec<String> {
        if self.has_modal() {
            return vec![
                "Current · Infrastructure Contribution".into(),
                "Enter Confirm contribution".into(),
                "Esc Cancel contribution".into(),
                String::new(),
                "The Rail Authority keeps ownership of the infrastructure.".into(),
            ];
        }

        vec![
            "Current · Rail Authority".into(),
            "↑↓ / jk Select infrastructure project".into(),
            "PgUp / PgDn Scroll project pipeline".into(),
            "f Contribute to selected project while it is in Funding".into(),
            String::new(),
            "The Authority controls public infrastructure; operator contributions are optional."
                .into(),
        ]
    }

    /// Routes one Authority-owned keyboard event. Global view navigation remains a Shell concern.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> AuthorityWorkspaceAction {
        if let Some(review) = self.contribution_review {
            return match key.code {
                KeyCode::Esc => {
                    self.contribution_review = None;
                    AuthorityWorkspaceAction::Notice(
                        "Infrastructure contribution cancelled; no changes were made.".into(),
                    )
                }
                KeyCode::Enter => AuthorityWorkspaceAction::Contribute {
                    project_id: review.project_id,
                    amount: review.amount,
                },
                _ => AuthorityWorkspaceAction::Continue,
            };
        }

        match key.code {
            KeyCode::Char('f' | 'F') => match self.selection.selected_project_id(state) {
                Some(project_id) => match ContributionReview::start(state, project_id) {
                    Ok(review) => {
                        self.contribution_review = Some(review);
                        AuthorityWorkspaceAction::ClearNotice
                    }
                    Err(message) => AuthorityWorkspaceAction::Notice(message.into()),
                },
                None => AuthorityWorkspaceAction::Notice(
                    "Select an infrastructure project first.".into(),
                ),
            },
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Char('j' | 'J' | 'k' | 'K') => {
                self.selection.handle_key(key.code, state);
                AuthorityWorkspaceAction::Continue
            }
            _ => AuthorityWorkspaceAction::Continue,
        }
    }

    /// Renders the active Authority content layer.
    pub fn render_dashboard(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        state: &GameState,
        now: UtcSeconds,
    ) {
        render_dashboard(frame, area, state, now, &mut self.selection);
    }

    /// Renders the compact text fallback used by the Shell.
    pub fn render_text(&self, state: &GameState, now: UtcSeconds) -> String {
        render(state, now)
    }

    /// Renders the focused contribution review when one is active.
    pub fn render_modal(&self, frame: &mut Frame, area: Rect, state: &GameState) {
        if let Some(review) = self.contribution_review {
            render_contribution_review(frame, area, state, review);
        }
    }

    /// Closes a contribution review after persistence succeeds and returns its amount.
    pub fn confirm_saved(&mut self) -> Money {
        let amount = self
            .contribution_review
            .map(|review| review.amount)
            .unwrap_or(Money::ZERO);
        self.contribution_review = None;
        amount
    }

    /// Clears transient Authority presentation state when starting a fresh game.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl ContributionReview {
    pub fn start(
        state: &GameState,
        project_id: InfrastructureProjectId,
    ) -> Result<Self, &'static str> {
        let project = state
            .region
            .rail_authority
            .infrastructure_projects
            .iter()
            .find(|project| project.id == project_id)
            .ok_or("The selected infrastructure project is no longer available.")?;
        if project.status != InfrastructureProjectStatus::Funding {
            return Err("Contributions are accepted only while a project is in Funding.");
        }
        let amount = project
            .funding
            .suggested_operator_contribution(state.player_company.funds)
            .map_err(|_| "The contribution amount could not be calculated.")?;
        if amount <= Money::ZERO {
            return Err(
                "No contribution can be made from the current Company Funds and project funding gap.",
            );
        }
        Ok(Self { project_id, amount })
    }
}

pub fn render_contribution_review(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    review: ContributionReview,
) {
    let project = state
        .region
        .rail_authority
        .infrastructure_projects
        .iter()
        .find(|project| project.id == review.project_id);
    let Some(project) = project else {
        return;
    };
    let card = modal::centered_rect(area, 70, 18);
    let modal_areas = modal::render_shell(
        frame,
        card,
        "Infrastructure Contribution",
        modal::shortcut_line(&[
            modal::ModalShortcut::enabled("Enter", modal::ModalAction::Contribute),
            modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        ]),
    );
    let remaining_cap = project
        .funding
        .remaining_operator_contribution_capacity()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());
    let mut projected_funding = project.funding.clone();
    projected_funding.operator_contributed = projected_funding
        .operator_contributed
        .checked_add(review.amount)
        .unwrap_or(projected_funding.operator_contributed);
    let projected_credit = projected_funding
        .operator_access_credit_value()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());
    let lines = vec![
        Line::from(vec![
            Span::styled("Project  ", theme::secondary()),
            Span::styled(project_scope(state, project), theme::primary_value()),
        ]),
        Line::from(""),
        money_line("Company Funds", state.player_company.funds),
        money_line("Contribution", review.amount),
        money_line("Already contributed", project.funding.operator_contributed),
        Line::from(vec![
            Span::styled("Access credit after opening  ", theme::secondary()),
            Span::styled(projected_credit, theme::success()),
        ]),
        Line::from(vec![
            Span::styled("Contribution capacity  ", theme::secondary()),
            Span::styled(remaining_cap, theme::primary_value()),
        ]),
        Line::from(""),
        Line::from("This is a 10% project-cost tranche, capped by the remaining funding gap,"),
        Line::from("the 20% operator cap, and current Company Funds."),
        Line::from("Contributing can close funding sooner but never shortens construction time."),
        Line::from(
            "After opening, 115% of contributed funds become finite access-fee credit on the project infrastructure.",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}

/// Renders the public Rail Authority as a dedicated read-only workspace.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    selection.synchronize(state);

    if area.width >= 100 && area.height >= 18 {
        render_wide(frame, area, state, now, selection);
    } else if area.height >= 14 {
        render_compact(frame, area, state, now, selection);
    } else {
        render_tiny(frame, area, state, now);
    }
}

fn render_wide(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let [summary_area, body_area] =
        Layout::vertical([Constraint::Length(10), Constraint::Fill(1)]).areas(area);
    let [finance_area, programme_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .areas(summary_area);
    render_finances(frame, finance_area, state, now);
    render_programme(frame, programme_area, state);

    let [projects_area, inspector_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .spacing(1)
            .areas(body_area);
    render_projects(frame, projects_area, state, now, selection, false);
    render_project_inspector(frame, inspector_area, state, now, selection);
}

fn render_compact(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut ProjectSelection,
) {
    let [summary_area, projects_area, inspector_area] = Layout::vertical([
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Fill(1),
    ])
    .areas(area);
    render_finances(frame, summary_area, state, now);
    render_projects(frame, projects_area, state, now, selection, true);
    render_project_inspector(frame, inspector_area, state, now, selection);
}

fn render_tiny(frame: &mut Frame, area: Rect, state: &GameState, now: UtcSeconds) {
    frame.render_widget(
        Paragraph::new(render(state, now))
            .block(panel_block("Rail Authority", false))
            .style(theme::panel())
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_finances(frame: &mut Frame, area: Rect, state: &GameState, now: UtcSeconds) {
    let authority = &state.region.rail_authority;
    let finances = &authority.finances;
    let available = finances
        .uncommitted_investment()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());

    let lines = vec![
        Line::from(vec![
            Span::styled("Authority  ", theme::secondary()),
            Span::styled(authority.name.clone(), theme::title()),
        ]),
        money_line("Treasury", finances.treasury),
        Line::from(vec![
            Span::styled("Available investment  ", theme::secondary()),
            Span::styled(available, theme::success()),
        ]),
        money_line("Maintenance reserve", finances.maintenance_reserve),
        money_line("Committed projects", finances.committed_investment),
        money_line(
            "Daily public allocation",
            finances.regional_public_allocation,
        ),
        money_line(
            "Access-fee revenue",
            finances.infrastructure_access_fee_revenue,
        ),
        finances
            .next_fiscal_period_at
            .map(|timestamp| schedule_line("Next fiscal period", timestamp, now))
            .unwrap_or_else(|| value_line("Next fiscal period", "Scheduling pending")),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Infrastructure Finances", false))
            .style(theme::panel()),
        area,
    );
}

fn render_programme(frame: &mut Frame, area: Rect, state: &GameState) {
    let authority = &state.region.rail_authority;
    let projects = &authority.infrastructure_projects;
    let active = authority.active_construction_count();
    let reserved = authority.reserved_construction_count();
    let open = projects
        .iter()
        .filter(|project| project.status == InfrastructureProjectStatus::Open)
        .count();
    let pipeline = projects
        .iter()
        .filter(|project| {
            !matches!(
                project.status,
                InfrastructureProjectStatus::Open
                    | InfrastructureProjectStatus::Rejected
                    | InfrastructureProjectStatus::Cancelled
            )
        })
        .count();

    let lines = vec![
        Line::from(vec![
            Span::styled("Network  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} stations · {} segments",
                    authority.rail_network.rail_stations.len(),
                    authority.rail_network.rail_lines.len()
                ),
                theme::primary_value(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Projects  ", theme::secondary()),
            Span::styled(
                format!("{} pipeline · {open} open", pipeline),
                theme::primary_value(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Construction slots  ", theme::secondary()),
            Span::styled(
                format!(
                    "{reserved}/{} reserved · {active} active · {} free",
                    authority.construction_capacity,
                    authority.construction_slots_remaining()
                ),
                if authority.construction_slots_remaining() == 0 {
                    theme::warning()
                } else {
                    theme::primary_value()
                },
            ),
        ]),
        status_count_line(projects, InfrastructureProjectStatus::Funding, "Funding"),
        status_count_line(
            projects,
            InfrastructureProjectStatus::Scheduled,
            "Scheduled",
        ),
        status_count_line(
            projects,
            InfrastructureProjectStatus::Construction,
            "Construction",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Development Programme", false))
            .style(theme::panel()),
        area,
    );
}

fn render_projects(
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
            .block(panel_block("Infrastructure Projects", false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let inner_height = area.height.saturating_sub(3);
    selection.set_page_size(usize::from(inner_height).max(1));
    selection.synchronize(state);

    let rows = projects
        .iter()
        .enumerate()
        .map(|(index, project)| {
            let scope = project_scope(state, project);
            let status = project_status(project.status);
            let next = project_next(state, project, now);
            if compact {
                Row::new(vec![
                    Cell::from(format!("{:02}", index + 1)),
                    Cell::from(scope),
                    Cell::from(status),
                ])
            } else {
                Row::new(vec![
                    Cell::from(format!("{:02}", index + 1)),
                    Cell::from(scope),
                    Cell::from(status).style(status_style(project.status)),
                    Cell::from(
                        Text::from(format::money(project.funding.estimated_cost)).right_aligned(),
                    ),
                    Cell::from(Text::from(funding_percent(project)).right_aligned()),
                    Cell::from(next),
                ])
            }
        })
        .collect::<Vec<_>>();

    let (header, widths) = if compact {
        (
            Row::new(["#", "Project", "Status"]).style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(14),
            ],
        )
    } else {
        (
            Row::new(vec![
                Cell::from("#"),
                Cell::from("Project"),
                Cell::from("Status"),
                Cell::from(Text::from("Cost").right_aligned()),
                Cell::from(Text::from("Funded").right_aligned()),
                Cell::from("Next milestone"),
            ])
            .style(theme::table_header()),
            vec![
                Constraint::Length(3),
                Constraint::Fill(2),
                Constraint::Length(14),
                Constraint::Length(13),
                Constraint::Length(8),
                Constraint::Fill(1),
            ],
        )
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(panel_block("Infrastructure Projects", false))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol(theme::SELECTION_MARKER)
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

fn render_project_inspector(
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

    let gap = project
        .funding
        .funding_gap()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("Project {:02}  ", index + 1), theme::title()),
            Span::styled(short_uuid(project.id), theme::secondary()),
        ]),
        Line::from(vec![
            Span::styled("Scope  ", theme::secondary()),
            Span::styled(project_scope(state, project), theme::primary_value()),
        ]),
        Line::from(vec![
            Span::styled("Status  ", theme::secondary()),
            Span::styled(project_status(project.status), status_style(project.status)),
        ]),
        Line::from(vec![
            Span::styled("Next  ", theme::secondary()),
            Span::styled(project_next(state, project, now), theme::primary_value()),
        ]),
    ];

    append_project_development_context(&mut lines, state, project, now);
    lines.push(Line::from(""));
    lines.extend([
        money_line("Estimated cost", project.funding.estimated_cost),
        money_line("Authority committed", project.funding.authority_committed),
        money_line(
            "Operator contribution",
            project.funding.operator_contributed,
        ),
        Line::from(vec![
            Span::styled("Funding gap  ", theme::secondary()),
            Span::styled(gap, theme::primary_value()),
        ]),
    ]);
    if project.funding.access_fee_credit_awarded > Money::ZERO {
        lines.push(money_line(
            "Access credit awarded",
            project.funding.access_fee_credit_awarded,
        ));
        lines.push(money_line(
            "Access credit remaining",
            project.funding.access_fee_credit_remaining,
        ));
    } else if project.funding.operator_contributed > Money::ZERO {
        if let Ok(projected_credit) = project.funding.operator_access_credit_value() {
            lines.push(money_line("Projected access credit", projected_credit));
        }
    }

    append_project_scope_details(&mut lines, state, project);
    lines.push(Line::from(""));
    append_timeline(&mut lines, state, project, now);

    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Selected Project", false))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn append_project_development_context(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
    now: UtcSeconds,
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

    lines.push(Line::from(""));
    lines.push(Line::styled("DEVELOPMENT CASE", theme::table_header()));
    lines.push(Line::from(vec![
        Span::styled("Requested by  ", theme::secondary()),
        Span::styled(
            format!(
                "{} Council",
                settlement_name(state, planned_station.settlement_id)
            ),
            theme::primary_value(),
        ),
    ]));

    if let Some(connection_station_id) = project_connection_station_id(project) {
        let maturity = local_rail_success_basis_points(
            &state.origin_destination_demand,
            connection_station_id,
        );
        lines.push(Line::from(vec![
            Span::styled("Nearby rail adoption  ", theme::secondary()),
            Span::styled(
                format!(
                    "{} · {}",
                    maturity_percent(maturity),
                    maturity_label(maturity)
                ),
                theme::primary_value(),
            ),
        ]));
    }

    if matches!(
        project.status,
        InfrastructureProjectStatus::Requested
            | InfrastructureProjectStatus::UnderReview
            | InfrastructureProjectStatus::Proposed
            | InfrastructureProjectStatus::Deferred
    ) {
        if let Some(score) =
            project_review_score_breakdown(&state.region, project, state.world_seed)
        {
            lines.push(Line::from(vec![
                Span::styled("Current case  ", theme::secondary()),
                Span::styled(
                    format!("{} / {}", score.total, AUTHORITY_APPROVAL_SCORE_THRESHOLD),
                    if score.total >= AUTHORITY_APPROVAL_SCORE_THRESHOLD {
                        theme::success()
                    } else {
                        theme::warning()
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Public value  ", theme::secondary()),
                Span::styled(
                    format!(
                        "+{} population · +{} demand · +{} network",
                        score.population, score.latent_demand, score.network_usefulness
                    ),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Regional / cost  ", theme::secondary()),
                Span::styled(
                    format!(
                        "+{} development · -{} construction",
                        score.regional_development, score.construction_cost_penalty
                    ),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Decision bands  ", theme::secondary()),
                Span::styled(
                    format!(
                        "approve ≥{} · defer {}–{} · reject <{}",
                        AUTHORITY_APPROVAL_SCORE_THRESHOLD,
                        AUTHORITY_REJECTION_SCORE_THRESHOLD,
                        AUTHORITY_APPROVAL_SCORE_THRESHOLD - 1,
                        AUTHORITY_REJECTION_SCORE_THRESHOLD,
                    ),
                    theme::secondary(),
                ),
            ]));
        }
    }

    match project.status {
        InfrastructureProjectStatus::Deferred => {
            lines.push(Line::from(vec![
                Span::styled("Reconsideration  ", theme::secondary()),
                Span::styled(deferred_next(state, project, now), theme::primary_value()),
            ]));
        }
        InfrastructureProjectStatus::Rejected => {
            lines.push(value_line("Reconsideration", "Not automatic"));
        }
        _ => {}
    }

    if project.timeline.reconsideration_count > 0 {
        lines.push(value_line(
            "Reconsiderations",
            &project.timeline.reconsideration_count.to_string(),
        ));
    }
}

fn append_project_scope_details(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
) {
    match &project.kind {
        InfrastructureProjectKind::NewLine {
            planned_stations,
            planned_lines,
        } => {
            let total_metres = planned_lines
                .iter()
                .map(|line| line.distance.metres())
                .sum::<u64>();
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "PLANNED INFRASTRUCTURE",
                theme::table_header(),
            ));
            if let Some(route) = new_line_route_label(state, planned_stations, planned_lines) {
                lines.push(Line::from(vec![
                    Span::styled("Route  ", theme::secondary()),
                    Span::styled(route, theme::primary_value()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("New stations  ", theme::secondary()),
                Span::styled(
                    planned_stations
                        .iter()
                        .map(|station| settlement_name(state, station.settlement_id))
                        .collect::<Vec<_>>()
                        .join(", "),
                    theme::primary_value(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("New track  ", theme::secondary()),
                Span::styled(format::distance(total_metres), theme::primary_value()),
            ]));
            if let Some(line) = planned_lines.first() {
                lines.push(Line::from(vec![
                    Span::styled("Initial capability  ", theme::secondary()),
                    Span::styled(
                        format!(
                            "{} km/h · {} track{} · {} · {} difficulty",
                            line.speed_limit.kilometres_per_hour(),
                            line.track_count.tracks(),
                            if line.track_count.tracks() == 1 {
                                ""
                            } else {
                                "s"
                            },
                            electrification_label(line.electrification),
                            difficulty_label(line.construction_difficulty)
                        ),
                        theme::primary_value(),
                    ),
                ]));
            }
        }
        InfrastructureProjectKind::SpeedUpgrade {
            rail_line_ids,
            target_speed_limit,
        } => lines.push(Line::from(format!(
            "{} segment(s) → {} km/h",
            rail_line_ids.len(),
            target_speed_limit.kilometres_per_hour()
        ))),
        InfrastructureProjectKind::DoubleTracking {
            rail_line_ids,
            target_track_count,
        } => lines.push(Line::from(format!(
            "{} segment(s) → {} tracks",
            rail_line_ids.len(),
            target_track_count.tracks()
        ))),
        InfrastructureProjectKind::Electrification { rail_line_ids } => lines.push(Line::from(
            format!("Electrify {} segment(s)", rail_line_ids.len()),
        )),
        InfrastructureProjectKind::Renewal { rail_line_ids } => lines.push(Line::from(format!(
            "Renew {} segment(s)",
            rail_line_ids.len()
        ))),
        InfrastructureProjectKind::StationUpgrade { rail_station_ids } => lines.push(Line::from(
            format!("Upgrade {} station(s)", rail_station_ids.len()),
        )),
    }
}

fn append_timeline(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    project: &InfrastructureProject,
    now: UtcSeconds,
) {
    lines.push(Line::styled("PROJECT TIMELINE", theme::table_header()));
    let request_label = if matches!(&project.kind, InfrastructureProjectKind::NewLine { .. }) {
        "Council request"
    } else {
        "Requested"
    };
    lines.push(timestamp_line(
        request_label,
        project.timeline.requested_at,
        now,
    ));
    if let Some(value) = project.timeline.approved_at {
        lines.push(timestamp_line("Approved", value, now));
    }
    if let Some(value) = project.timeline.funding_completed_at {
        lines.push(timestamp_line("Funded", value, now));
    }

    lines.push(Line::from(""));
    match project.status {
        InfrastructureProjectStatus::Requested => {
            lines.push(Line::styled("REQUESTED", theme::table_header()));
            lines.push(value_line("Next", "Awaiting review"));
        }
        InfrastructureProjectStatus::UnderReview => {
            lines.push(Line::styled("REVIEW", theme::table_header()));
            if let Some(value) = project.timeline.review_started_at {
                lines.push(timestamp_line("Started", value, now));
            }
            lines.push(value_line("Next", "Decision pending"));
        }
        InfrastructureProjectStatus::Proposed => {
            lines.push(Line::styled("PROPOSAL", theme::table_header()));
            if let Some(value) = project.timeline.proposed_at {
                lines.push(timestamp_line("Proposed", value, now));
            }
            lines.push(value_line("Next", "Authority decision"));
        }
        InfrastructureProjectStatus::Approved => {
            lines.push(Line::styled("APPROVED", theme::table_header()));
            lines.push(value_line("Next", "Awaiting funding slot"));
        }
        InfrastructureProjectStatus::Deferred => {
            lines.push(Line::styled("DEFERRED", theme::table_header()));
            if let Some(value) = project.timeline.deferred_at {
                lines.push(timestamp_line("Deferred", value, now));
            }
            lines.push(value_line("Next", &deferred_next(state, project, now)));
        }
        InfrastructureProjectStatus::Rejected => {
            lines.push(Line::styled("REJECTED", theme::table_header()));
            lines.push(value_line("Next", "No automatic reconsideration"));
        }
        InfrastructureProjectStatus::Funding => {
            lines.push(Line::styled("FUNDING", theme::table_header()));
            let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
            let committed = project
                .funding
                .total_funded()
                .map(|money| i128::from(money.cents()).max(0))
                .unwrap_or(0);
            let percent = if estimated == 0 {
                0
            } else {
                committed
                    .saturating_mul(100)
                    .saturating_div(estimated)
                    .min(100) as u8
            };
            lines.push(progress_line("Progress", percent));
            if let Ok(gap) = project.funding.funding_gap() {
                lines.push(money_line("Remaining", gap));
            }
        }
        InfrastructureProjectStatus::Scheduled => {
            lines.push(Line::styled("CONSTRUCTION QUEUE", theme::table_header()));
            if let Some(value) = project.timeline.scheduled_start_at {
                lines.push(schedule_line("Starts", value, now));
            } else {
                lines.push(value_line("Starts", "Awaiting construction slot"));
            }
        }
        InfrastructureProjectStatus::Construction => {
            lines.push(Line::styled("CONSTRUCTION", theme::table_header()));
            if let Some(started) = project.timeline.construction_started_at {
                lines.push(timestamp_line("Started", started, now));
                if let Some(completion) = project.timeline.planned_completion_at {
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

                    lines.push(duration_line("Duration", duration));
                    lines.push(Line::from(vec![
                        Span::styled("Opens  ", theme::secondary()),
                        Span::styled(
                            format_project_timestamp(completion, now),
                            theme::primary_value(),
                        ),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Remaining  ", theme::secondary()),
                        Span::styled(
                            construction_remaining_duration(remaining),
                            theme::primary_value(),
                        ),
                    ]));
                    lines.push(progress_line("Progress", progress));
                }
            }
        }
        InfrastructureProjectStatus::Open => {
            lines.push(Line::styled("OPEN", theme::table_header()));
            if let Some(value) = project.timeline.completed_at {
                lines.push(Line::from(vec![
                    Span::styled("Opened  ", theme::secondary()),
                    Span::styled(format_project_timestamp(value, now), theme::primary_value()),
                ]));
            }
        }
        InfrastructureProjectStatus::Cancelled => {
            lines.push(Line::styled("CANCELLED", theme::table_header()));
            if let Some(value) = project.timeline.cancelled_at {
                lines.push(timestamp_line("Cancelled", value, now));
            }
        }
    }
}

fn money_line(label: &str, value: Money) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(format::money(value), theme::primary_value()),
    ])
}

fn status_count_line(
    projects: &[InfrastructureProject],
    status: InfrastructureProjectStatus,
    label: &str,
) -> Line<'static> {
    let count = projects
        .iter()
        .filter(|project| project.status == status)
        .count();
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(count.to_string(), theme::primary_value()),
    ])
}

fn project_scope(state: &GameState, project: &InfrastructureProject) -> String {
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

fn new_line_route_label(
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

fn station_name(
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

fn settlement_name(state: &GameState, settlement_id: crate::model::SettlementId) -> String {
    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| "Unknown settlement".into())
}

fn project_status(status: InfrastructureProjectStatus) -> &'static str {
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

fn status_style(status: InfrastructureProjectStatus) -> ratatui::style::Style {
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

fn funding_percent(project: &InfrastructureProject) -> String {
    let estimated = i128::from(project.funding.estimated_cost.cents()).max(0);
    let committed = project
        .funding
        .total_funded()
        .map(|money| i128::from(money.cents()).max(0))
        .unwrap_or(0);
    if estimated == 0 {
        return "—".into();
    }
    let percent = committed
        .saturating_mul(100)
        .saturating_div(estimated)
        .min(100);
    format!("{percent}%")
}

fn project_next(state: &GameState, project: &InfrastructureProject, now: UtcSeconds) -> String {
    match project.status {
        InfrastructureProjectStatus::Requested => "Council request · review pending".into(),
        InfrastructureProjectStatus::UnderReview => "Authority review in progress".into(),
        InfrastructureProjectStatus::Proposed => "Authority decision pending".into(),
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
                    format!("Still needs {}", format::money(gap))
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

fn deferred_next(state: &GameState, project: &InfrastructureProject, now: UtcSeconds) -> String {
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

fn maturity_percent(basis_points: u16) -> String {
    format!("{}%", u32::from(basis_points).saturating_add(50) / 100)
}

fn maturity_label(basis_points: u16) -> &'static str {
    match basis_points {
        0..=3_499 => "Emerging",
        3_500..=5_999 => "Growing",
        6_000..=8_499 => "Established",
        _ => "Mature",
    }
}

fn timestamp_line(label: &str, timestamp: UtcSeconds, now: UtcSeconds) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(relative_time(timestamp, now), theme::primary_value()),
    ])
}

fn value_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(value.to_string(), theme::primary_value()),
    ])
}

fn schedule_line(label: &str, timestamp: UtcSeconds, now: UtcSeconds) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(
            format_project_timestamp(timestamp, now),
            theme::primary_value(),
        ),
        Span::styled(" · ", theme::secondary()),
        Span::styled(relative_time(timestamp, now), theme::primary_value()),
    ])
}

fn duration_line(label: &str, seconds: u64) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled(compact_duration(seconds), theme::primary_value()),
    ])
}

fn progress_line(label: &str, percent: u8) -> Line<'static> {
    let percent = percent.min(100);
    let filled = usize::from(percent).saturating_mul(10).saturating_add(50) / 100;
    let empty = 10usize.saturating_sub(filled);
    Line::from(vec![
        Span::styled(format!("{label}  "), theme::secondary()),
        Span::styled("█".repeat(filled), theme::warning()),
        Span::styled("░".repeat(empty), theme::secondary()),
        Span::styled(format!("  {percent}%"), theme::primary_value()),
    ])
}

fn format_project_timestamp(timestamp: UtcSeconds, now: UtcSeconds) -> String {
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

fn utc_date_time(timestamp: UtcSeconds) -> (i64, i64, i64, i64, i64) {
    let unix = timestamp.unix_seconds();
    let days = unix.div_euclid(86_400);
    let seconds_of_day = unix.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_unix_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    (year, month, day, hour, minute)
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
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

fn relative_time(timestamp: UtcSeconds, now: UtcSeconds) -> String {
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

fn construction_remaining_duration(seconds: u64) -> String {
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

fn compact_duration(seconds: u64) -> String {
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

fn short_uuid(id: InfrastructureProjectId) -> String {
    id.to_string().chars().take(8).collect()
}

fn electrification_label(value: Electrification) -> &'static str {
    match value {
        Electrification::None => "non-electrified",
        Electrification::Electric => "electric",
    }
}

fn difficulty_label(value: ConstructionDifficulty) -> &'static str {
    match value {
        ConstructionDifficulty::Low => "low",
        ConstructionDifficulty::Moderate => "moderate",
        ConstructionDifficulty::High => "high",
    }
}

/// Compact textual fallback used by very small terminals and tests.
pub fn render(state: &GameState, now: UtcSeconds) -> String {
    let authority = &state.region.rail_authority;
    let finances = &authority.finances;
    let available = finances
        .uncommitted_investment()
        .map(format::money)
        .unwrap_or_else(|_| "—".into());
    let mut output = String::new();
    writeln!(output, "{}", authority.name).expect("writing to String cannot fail");
    writeln!(output, "Treasury: {}", format::money(finances.treasury))
        .expect("writing to String cannot fail");
    writeln!(output, "Available investment: {available}").expect("writing to String cannot fail");
    writeln!(
        output,
        "Maintenance reserve: {}",
        format::money(finances.maintenance_reserve)
    )
    .expect("writing to String cannot fail");
    if let Some(next) = finances.next_fiscal_period_at {
        writeln!(output, "Next fiscal period: {}", relative_time(next, now))
            .expect("writing to String cannot fail");
    }
    writeln!(
        output,
        "Construction slots: {}/{} reserved",
        authority.reserved_construction_count(),
        authority.construction_capacity
    )
    .expect("writing to String cannot fail");
    writeln!(output, "Projects:").expect("writing to String cannot fail");
    for (index, project) in authority.infrastructure_projects.iter().enumerate() {
        writeln!(
            output,
            "{:02}  {:<14}  {}  {}",
            index + 1,
            project_status(project.status),
            project_scope(state, project),
            project_next(state, project, now)
        )
        .expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{MarketMaturity, UtcSeconds},
        sim::{authority::advance_rail_authority, world::create_new_game},
    };

    use super::{
        AuthorityWorkspace, AuthorityWorkspaceAction, ProjectSelection, compact_duration,
        construction_remaining_duration, format_project_timestamp, render,
    };

    fn establish_rail_markets(state: &mut crate::model::GameState) {
        for pool in &mut state.origin_destination_demand {
            pool.market_maturity = MarketMaturity::full();
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn authority_render_exposes_budget_and_project_pipeline() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let world_seed = state.world_seed;
        establish_rail_markets(&mut state);
        advance_rail_authority(
            &mut state.region,
            world_seed,
            &state.origin_destination_demand,
            &state.rules.authority,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();

        let output = render(&state, UtcSeconds::from_unix_seconds(0));
        assert!(output.contains("Treasury:"));
        assert!(output.contains("Available investment:"));
        assert!(output.contains("Next fiscal period:"));
        assert!(output.contains("Projects:"));
        assert!(output.contains("Council request"));
        assert!(output.contains(" → "));
    }

    #[test]
    fn deferred_project_render_exposes_reconsideration_gates() {
        let now = UtcSeconds::from_unix_seconds(0);
        let mut state = create_new_game(42, "One More Prime", now);
        let world_seed = state.world_seed;
        establish_rail_markets(&mut state);
        advance_rail_authority(
            &mut state.region,
            world_seed,
            &state.origin_destination_demand,
            &state.rules.authority,
            now,
        )
        .unwrap();
        for pool in &mut state.origin_destination_demand {
            pool.market_maturity = MarketMaturity::from_basis_points(2_500).unwrap();
        }
        let project = state
            .region
            .rail_authority
            .infrastructure_projects
            .first_mut()
            .expect("planning should create an infrastructure project");
        project.status = crate::model::InfrastructureProjectStatus::Deferred;
        project.timeline.deferred_at = Some(now);

        let output = render(&state, now);
        assert!(output.contains("Eligible in 1d · needs 55% adoption"));
    }

    #[test]
    fn formats_project_times_without_seconds() {
        let now = UtcSeconds::from_unix_seconds(1_700_000_000);
        assert_eq!(
            format_project_timestamp(UtcSeconds::from_unix_seconds(1_700_003_600), now),
            "Today 23:13 UTC"
        );
        assert_eq!(compact_duration(8_110), "2h 15m");
        assert_eq!(compact_duration(42), "42s");
        assert_eq!(construction_remaining_duration(1_811), "30m");
        assert_eq!(construction_remaining_duration(1_800), "30m 00s");
        assert_eq!(construction_remaining_duration(1_742), "29m 02s");
        assert_eq!(construction_remaining_duration(42), "42s");
    }

    #[test]
    fn project_selection_tracks_project_identity() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let world_seed = state.world_seed;
        establish_rail_markets(&mut state);
        advance_rail_authority(
            &mut state.region,
            world_seed,
            &state.origin_destination_demand,
            &state.rules.authority,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();
        let mut selection = ProjectSelection::default();
        selection.handle_key(KeyCode::Down, &state);
        assert!(selection.selected_project(&state).is_some());
    }

    #[test]
    fn authority_workspace_owns_project_navigation_and_missing_contribution_feedback() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        state.region.rail_authority.infrastructure_projects.clear();
        let mut workspace = AuthorityWorkspace::default();
        let shortcuts = workspace.shortcuts(&state, false, true);
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].key, "↑↓");
        assert!(!shortcuts[0].enabled);

        assert_eq!(
            workspace.handle_key(key(KeyCode::Down), &state),
            AuthorityWorkspaceAction::Continue
        );
        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('f')), &state),
            AuthorityWorkspaceAction::Notice("Select an infrastructure project first.".into())
        );
        assert!(!workspace.has_modal());
    }

    #[test]
    fn authority_workspace_owns_contribution_review_lifecycle() {
        let mut state = create_new_game(42, "One More Prime", UtcSeconds::from_unix_seconds(0));
        let world_seed = state.world_seed;
        establish_rail_markets(&mut state);
        advance_rail_authority(
            &mut state.region,
            world_seed,
            &state.origin_destination_demand,
            &state.rules.authority,
            UtcSeconds::from_unix_seconds(0),
        )
        .unwrap();
        let project = state
            .region
            .rail_authority
            .infrastructure_projects
            .first_mut()
            .expect("planning should create an infrastructure project");
        project.status = crate::model::InfrastructureProjectStatus::Funding;

        let mut workspace = AuthorityWorkspace::default();
        assert!(workspace.can_contribute(&state));
        assert!(
            workspace
                .shortcuts(&state, false, true)
                .iter()
                .any(|shortcut| shortcut.key == "F" && shortcut.enabled)
        );
        assert_eq!(
            workspace.handle_key(key(KeyCode::Char('f')), &state),
            AuthorityWorkspaceAction::ClearNotice
        );
        assert!(workspace.has_modal());
        let modal_shortcuts = workspace.shortcuts(&state, false, true);
        assert_eq!(
            modal_shortcuts
                .iter()
                .map(|shortcut| shortcut.key.as_str())
                .collect::<Vec<_>>(),
            vec!["Enter", "Esc"]
        );
        assert_eq!(
            workspace.help_lines().first().map(String::as_str),
            Some("Current · Infrastructure Contribution")
        );

        let action = workspace.handle_key(key(KeyCode::Enter), &state);
        assert!(matches!(
            action,
            AuthorityWorkspaceAction::Contribute { amount, .. } if amount > crate::model::Money::ZERO
        ));
        assert!(workspace.has_modal());

        let amount = workspace.confirm_saved();
        assert!(amount > crate::model::Money::ZERO);
        assert!(!workspace.has_modal());
    }
}
