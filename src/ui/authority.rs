//! Rail Authority infrastructure programme presentation.
//!
//! This workspace owns Authority presentation state and keyboard interaction.
//! Rendering, presentation analytics, contribution review, and formatting live
//! in focused submodules so the dashboard redesign can evolve without growing
//! another monolithic UI file.

mod analytics;
mod contribution;
mod dashboard;
mod format;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect, widgets::TableState};

use crate::model::{
    GameState, InfrastructureProject, InfrastructureProjectId, InfrastructureProjectStatus, Money,
    UtcSeconds,
};

pub use contribution::{ContributionReview, render_contribution_review};
pub use dashboard::{render_dashboard, render_text as render};
pub(crate) use format::access_discount_label;

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


#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{DurationSeconds, MarketMaturity, UtcSeconds},
        sim::{authority::advance_rail_authority, world::create_new_game},
    };

    use super::{
        AuthorityWorkspace, AuthorityWorkspaceAction, ProjectSelection, access_discount_label,
        render,
        format::{
            compact_duration, construction_remaining_duration, format_project_timestamp,
            planning_stage_next,
        },
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
        assert!(output.contains("Review in 1h"));
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
    fn planning_stage_countdown_exposes_configured_cadence() {
        let started = UtcSeconds::from_unix_seconds(1_000);
        let delay = DurationSeconds::from_seconds(60 * 60);

        assert_eq!(
            planning_stage_next("Review", started, delay, started),
            "Review in 1h"
        );
        assert_eq!(
            planning_stage_next(
                "Review",
                started,
                delay,
                UtcSeconds::from_unix_seconds(4_600),
            ),
            "Review ready"
        );
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
    fn access_discount_labels_show_remaining_time_and_expiry_state() {
        let discount = crate::model::InfrastructureAccessDiscount {
            basis_points: 5_000,
            expires_at: UtcSeconds::from_unix_seconds(8 * 86_400),
        };

        assert_eq!(
            access_discount_label(
                discount,
                UtcSeconds::from_unix_seconds(2 * 86_400 + 11 * 3_600)
            ),
            "50% · 5d 13h remaining"
        );
        assert_eq!(
            access_discount_label(discount, UtcSeconds::from_unix_seconds(8 * 86_400)),
            "50% · expired"
        );
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
