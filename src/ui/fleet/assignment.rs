//! Passenger Service assignment workflow opened from the Fleet workspace.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::model::{GameState, ServiceDirectionMode, ServiceId, TrainId, TrainStatus};

use super::{modal, theme};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServiceAssignmentAction {
    Continue,
    Cancel,
    Assign {
        train_id: TrainId,
        service_id: ServiceId,
    },
    Unassign {
        train_id: TrainId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ServiceAssignmentFlow {
    train_id: TrainId,
    selected_index: usize,
    rejection: Option<String>,
}

impl ServiceAssignmentFlow {
    pub(super) fn start(state: &GameState, train_id: TrainId) -> Result<Self, String> {
        let train = state
            .player_company
            .fleet
            .trains
            .iter()
            .find(|train| train.id == train_id)
            .ok_or_else(|| format!("Train {:02} is no longer in the Fleet.", train_id.get()))?;
        if matches!(&train.status, TrainStatus::Travelling { .. }) {
            return Err(format!(
                "Train {:02} is travelling; change its Service assignment after arrival.",
                train_id.get()
            ));
        }

        let current_service = state.player_company.fleet.assigned_service_id(train_id);
        let selected_index = current_service
            .and_then(|service_id| {
                state
                    .player_company
                    .passenger_services
                    .iter()
                    .position(|service| service.id == service_id)
                    .map(|index| index + 1)
            })
            .unwrap_or(0);

        Ok(Self {
            train_id,
            selected_index,
            rejection: None,
        })
    }

    pub(super) fn handle_key(
        &mut self,
        key: KeyCode,
        state: &GameState,
    ) -> ServiceAssignmentAction {
        let option_count = state.player_company.passenger_services.len() + 1;
        match key {
            KeyCode::Esc => ServiceAssignmentAction::Cancel,
            KeyCode::PageUp => {
                self.selected_index = self.selected_index.saturating_sub(8);
                self.rejection = None;
                ServiceAssignmentAction::Continue
            }
            KeyCode::PageDown => {
                self.selected_index = (self.selected_index + 8).min(option_count.saturating_sub(1));
                self.rejection = None;
                ServiceAssignmentAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k' | 'K') => {
                self.selected_index = self.selected_index.saturating_sub(1);
                self.rejection = None;
                ServiceAssignmentAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j' | 'J') => {
                self.selected_index = (self.selected_index + 1).min(option_count.saturating_sub(1));
                self.rejection = None;
                ServiceAssignmentAction::Continue
            }
            KeyCode::Enter if self.selected_index == 0 => {
                if state
                    .player_company
                    .fleet
                    .assigned_service_id(self.train_id)
                    .is_some()
                {
                    ServiceAssignmentAction::Unassign {
                        train_id: self.train_id,
                    }
                } else {
                    ServiceAssignmentAction::Continue
                }
            }
            KeyCode::Enter => {
                let Some(service) = state
                    .player_company
                    .passenger_services
                    .get(self.selected_index.saturating_sub(1))
                else {
                    return ServiceAssignmentAction::Continue;
                };
                if state
                    .player_company
                    .fleet
                    .assigned_service_id(self.train_id)
                    == Some(service.id)
                {
                    ServiceAssignmentAction::Continue
                } else {
                    ServiceAssignmentAction::Assign {
                        train_id: self.train_id,
                        service_id: service.id,
                    }
                }
            }
            _ => ServiceAssignmentAction::Continue,
        }
    }

    pub(super) fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    pub(super) fn footer_shortcuts(
        &self,
        state: &GameState,
        compact: bool,
    ) -> Vec<(&'static str, &'static str, bool)> {
        let assigned = state
            .player_company
            .fleet
            .assigned_service_id(self.train_id);
        let (action, action_enabled) = if self.selected_index == 0 {
            if assigned.is_some() {
                ("Unassign", true)
            } else {
                ("Unassign", false)
            }
        } else if state
            .player_company
            .passenger_services
            .get(self.selected_index.saturating_sub(1))
            .is_some_and(|service| assigned == Some(service.id))
        {
            ("Assign", false)
        } else {
            ("Assign", true)
        };
        vec![
            ("Esc", "Cancel", true),
            ("Enter", action, action_enabled),
            (if compact { "↑↓" } else { "↑↓/JK" }, "Service", true),
        ]
    }
}

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &ServiceAssignmentFlow,
) {
    let title = format!("Assign Service · Train {:02}", flow.train_id.get());
    let assigned = state
        .player_company
        .fleet
        .assigned_service_id(flow.train_id);
    let (enter_action, enter_enabled) = if flow.selected_index == 0 {
        (modal::ModalAction::Unassign, assigned.is_some())
    } else if state
        .player_company
        .passenger_services
        .get(flow.selected_index.saturating_sub(1))
        .is_some_and(|service| assigned == Some(service.id))
    {
        (modal::ModalAction::Assign, false)
    } else {
        (modal::ModalAction::Assign, true)
    };
    let enter_shortcut = if enter_enabled {
        modal::ModalShortcut::enabled("Enter", enter_action)
    } else {
        modal::ModalShortcut::disabled("Enter", enter_action)
    };
    let footer = modal::shortcut_line(&[
        modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        enter_shortcut,
        modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Service),
    ]);
    let modal_areas = modal::render_shell(frame, area, &title, footer);
    let [context_area, table_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(4)]).areas(modal_areas.body);

    let context = if let Some(rejection) = &flow.rejection {
        vec![
            Line::styled("Assignment unavailable", theme::warning()),
            Line::styled(rejection.clone(), theme::error()),
        ]
    } else if state.player_company.passenger_services.is_empty() {
        vec![
            Line::styled("No Passenger Services", theme::title()),
            Line::styled(
                "Create a Service before assigning this Train.",
                theme::secondary(),
            ),
        ]
    } else {
        let selection_hint = if flow.selected_index == 0 {
            if let Some(service_id) = assigned {
                format!(
                    "Currently R{} · Enter unassigns this Train.",
                    service_id.get()
                )
            } else {
                "Already unassigned · choose a Service to allocate this Train.".to_owned()
            }
        } else if let Some(service) = state
            .player_company
            .passenger_services
            .get(flow.selected_index.saturating_sub(1))
        {
            if assigned == Some(service.id) {
                format!(
                    "R{} is the current assignment · no change required.",
                    service.id.get()
                )
            } else if let Some(current_service_id) = assigned {
                format!(
                    "Currently R{} · Enter reassigns this Train to R{}.",
                    current_service_id.get(),
                    service.id.get()
                )
            } else {
                format!(
                    "Unassigned · Enter assigns this Train to R{}.",
                    service.id.get()
                )
            }
        } else {
            "Select a Passenger Service for this Train.".to_owned()
        };
        vec![
            Line::styled(
                "Select the Passenger Service for this Train.",
                theme::title(),
            ),
            Line::styled(selection_hint, theme::secondary()),
        ]
    };
    frame.render_widget(
        Paragraph::new(context)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        context_area,
    );

    let mut rows = vec![Row::new(vec![
        Cell::from("—"),
        Cell::from("Unassigned"),
        Cell::from("No Passenger Service"),
        Cell::from(if assigned.is_none() { "Current" } else { "" }),
    ])];
    rows.extend(
        state
            .player_company
            .passenger_services
            .iter()
            .map(|service| {
                let route = match (
                    service.stop_station_ids.first(),
                    service.stop_station_ids.last(),
                ) {
                    (Some(origin), Some(destination)) => format!(
                        "{} {} {}",
                        station_label(state, *origin),
                        match service.direction_mode {
                            ServiceDirectionMode::BothDirections => "↔",
                            ServiceDirectionMode::ForwardOnly => "→",
                        },
                        station_label(state, *destination),
                    ),
                    _ => "Invalid route".to_owned(),
                };
                Row::new(vec![
                    Cell::from(format!("R{}", service.id.get())),
                    Cell::from(service.display_name()),
                    Cell::from(route),
                    Cell::from(if assigned == Some(service.id) {
                        "Current"
                    } else {
                        ""
                    }),
                ])
            }),
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(20),
            Constraint::Min(24),
            Constraint::Length(10),
        ],
    )
    .header(Row::new(["Service", "Name", "Route", "Assignment"]).style(theme::table_header()))
    .style(theme::panel())
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("› ")
    .highlight_spacing(HighlightSpacing::Always);
    let mut table_state = TableState::default();
    table_state.select(Some(flow.selected_index));
    frame.render_stateful_widget(table, table_area, &mut table_state);
}

fn station_label(state: &GameState, station_id: crate::model::RailStationId) -> String {
    let Some(station) = state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .find(|station| station.id == station_id)
    else {
        return format!("Station {}", station_id.get());
    };

    state
        .region
        .settlements
        .iter()
        .find(|settlement| settlement.id == station.settlement_id)
        .map(|settlement| settlement.name.clone())
        .unwrap_or_else(|| format!("Station {}", station_id.get()))
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train,
            services::{assign_train_to_service, create_service},
            world::create_new_game,
        },
    };

    use super::{ServiceAssignmentAction, ServiceAssignmentFlow};

    #[test]
    fn no_op_assignment_choices_stay_open_and_clear_old_feedback_on_navigation() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(10_000_000);
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        let mut flow = ServiceAssignmentFlow::start(&state, train_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceAssignmentAction::Continue
        );

        assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let mut flow = ServiceAssignmentFlow::start(&state, train_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceAssignmentAction::Continue
        );
        flow.reject("old assignment error");
        assert_eq!(
            flow.handle_key(KeyCode::Up, &state),
            ServiceAssignmentAction::Continue
        );
        assert!(flow.rejection.is_none());
    }

    #[test]
    fn fleet_assignment_can_assign_and_unassign_the_selected_train() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(10_000_000);
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        let mut flow = ServiceAssignmentFlow::start(&state, train_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Down, &state),
            ServiceAssignmentAction::Continue
        );
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceAssignmentAction::Assign {
                train_id,
                service_id,
            }
        );

        assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let mut flow = ServiceAssignmentFlow::start(&state, train_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Up, &state),
            ServiceAssignmentAction::Continue
        );
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceAssignmentAction::Unassign { train_id }
        );
    }
}
