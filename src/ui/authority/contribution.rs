//! Operator contribution review for Rail Authority infrastructure projects.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    model::{
        GameState, InfrastructureProjectId, InfrastructureProjectStatus, Money,
        PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS,
        PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS,
    },
    ui::{format as ui_format, modal, theme},
};

use super::format::{money_line, project_scope};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContributionReview {
    pub project_id: InfrastructureProjectId,
    pub amount: Money,
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
        .map(ui_format::money)
        .unwrap_or_else(|_| "—".into());
    let discount_percent = u32::from(PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_BASIS_POINTS) / 100;
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
            Span::styled("Access discount after opening  ", theme::secondary()),
            Span::styled(
                format!(
                    "{discount_percent}% for {} fiscal days",
                    PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS
                ),
                theme::success(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Contribution capacity  ", theme::secondary()),
            Span::styled(remaining_cap, theme::primary_value()),
        ]),
        Line::from(""),
        Line::from("This is a 10% project-cost tranche, capped by the remaining funding gap,"),
        Line::from("the 20% operator cap, and current Company Funds."),
        Line::from("Contributing can close funding sooner but never shortens construction time."),
        Line::from(format!(
            "After opening, contributed infrastructure receives a {discount_percent}% access-fee discount for {} fiscal days.",
            PROVISIONAL_OPERATOR_ACCESS_DISCOUNT_DURATION_DAYS
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        modal_areas.body,
    );
}
