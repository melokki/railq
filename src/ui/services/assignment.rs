//! Train assignment workflow opened from the Passenger Services workspace.

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
pub(super) enum ServiceTrainAssignmentAction {
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
pub(super) struct ServiceTrainAssignmentFlow {
    service_id: ServiceId,
    selected_index: usize,
    rejection: Option<String>,
}

impl ServiceTrainAssignmentFlow {
    pub(super) fn start(state: &GameState, service_id: ServiceId) -> Result<Self, String> {
        if !state
            .player_company
            .passenger_services
            .iter()
            .any(|service| service.id == service_id)
        {
            return Err(format!(
                "Passenger Service R{} is no longer available.",
                service_id.get()
            ));
        }

        let selected_index = state
            .player_company
            .fleet
            .trains
            .iter()
            .position(|train| {
                state.player_company.fleet.assigned_service_id(train.id) == Some(service_id)
            })
            .unwrap_or(0);

        Ok(Self {
            service_id,
            selected_index,
            rejection: None,
        })
    }

    pub(super) fn handle_key(
        &mut self,
        key: KeyCode,
        state: &GameState,
    ) -> ServiceTrainAssignmentAction {
        let option_count = state.player_company.fleet.trains.len();
        match key {
            KeyCode::Esc => ServiceTrainAssignmentAction::Cancel,
            KeyCode::PageUp => {
                self.selected_index = self.selected_index.saturating_sub(8);
                self.rejection = None;
                ServiceTrainAssignmentAction::Continue
            }
            KeyCode::PageDown => {
                if option_count > 0 {
                    self.selected_index =
                        (self.selected_index + 8).min(option_count.saturating_sub(1));
                }
                self.rejection = None;
                ServiceTrainAssignmentAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k' | 'K') => {
                self.selected_index = self.selected_index.saturating_sub(1);
                self.rejection = None;
                ServiceTrainAssignmentAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j' | 'J') => {
                if option_count > 0 {
                    self.selected_index =
                        (self.selected_index + 1).min(option_count.saturating_sub(1));
                }
                self.rejection = None;
                ServiceTrainAssignmentAction::Continue
            }
            KeyCode::Enter => {
                let Some(train) = state.player_company.fleet.trains.get(self.selected_index) else {
                    return ServiceTrainAssignmentAction::Continue;
                };
                if matches!(&train.status, TrainStatus::Travelling { .. }) {
                    self.rejection = Some(format!(
                        "Train {:02} is travelling; change its Service assignment after arrival.",
                        train.id.get()
                    ));
                    return ServiceTrainAssignmentAction::Continue;
                }

                if state.player_company.fleet.assigned_service_id(train.id) == Some(self.service_id)
                {
                    self.rejection = Some(format!(
                        "Train {:02} is already assigned to this Service; press U to unassign it.",
                        train.id.get()
                    ));
                    ServiceTrainAssignmentAction::Continue
                } else {
                    ServiceTrainAssignmentAction::Assign {
                        train_id: train.id,
                        service_id: self.service_id,
                    }
                }
            }
            KeyCode::Char('u' | 'U') => {
                let Some(train) = state.player_company.fleet.trains.get(self.selected_index) else {
                    return ServiceTrainAssignmentAction::Continue;
                };
                if matches!(&train.status, TrainStatus::Travelling { .. }) {
                    self.rejection = Some(format!(
                        "Train {:02} is travelling; unassign it after arrival.",
                        train.id.get()
                    ));
                    return ServiceTrainAssignmentAction::Continue;
                }
                if state.player_company.fleet.assigned_service_id(train.id) != Some(self.service_id)
                {
                    self.rejection = Some(format!(
                        "Train {:02} is not assigned to this Service.",
                        train.id.get()
                    ));
                    return ServiceTrainAssignmentAction::Continue;
                }
                ServiceTrainAssignmentAction::Unassign { train_id: train.id }
            }
            _ => ServiceTrainAssignmentAction::Continue,
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
        let selected = self.selected_train(state);
        let travelling =
            selected.is_some_and(|train| matches!(&train.status, TrainStatus::Travelling { .. }));
        let current = selected.is_some_and(|train| {
            state.player_company.fleet.assigned_service_id(train.id) == Some(self.service_id)
        });
        let enter_action = if selected.is_some_and(|train| {
            state
                .player_company
                .fleet
                .assigned_service_id(train.id)
                .is_some_and(|service_id| service_id != self.service_id)
        }) {
            "Reassign"
        } else {
            "Assign"
        };

        vec![
            ("Esc", "Cancel", true),
            (
                if compact { "↑↓" } else { "↑↓/JK" },
                "Train",
                !state.player_company.fleet.trains.is_empty(),
            ),
            (
                "Enter",
                enter_action,
                selected.is_some() && !travelling && !current,
            ),
            ("U", "Unassign", current && !travelling),
        ]
    }

    fn selected_train<'a>(&self, state: &'a GameState) -> Option<&'a crate::model::Train> {
        state.player_company.fleet.trains.get(self.selected_index)
    }
}

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    flow: &ServiceTrainAssignmentFlow,
) {
    let service = state
        .player_company
        .passenger_services
        .iter()
        .find(|service| service.id == flow.service_id);
    let title = service
        .map(|service| format!("Assign Trains · {}", service.display_name()))
        .unwrap_or_else(|| format!("Assign Trains · R{}", flow.service_id.get()));
    let (enter_action, enter_enabled, unassign_enabled) = flow
        .selected_train(state)
        .map(|train| {
            if matches!(&train.status, TrainStatus::Travelling { .. }) {
                (modal::ModalAction::Locked, false, false)
            } else if state.player_company.fleet.assigned_service_id(train.id)
                == Some(flow.service_id)
            {
                (modal::ModalAction::Assign, false, true)
            } else if state
                .player_company
                .fleet
                .assigned_service_id(train.id)
                .is_some()
            {
                (modal::ModalAction::Reassign, true, false)
            } else {
                (modal::ModalAction::Assign, true, false)
            }
        })
        .unwrap_or((modal::ModalAction::Close, false, false));
    let enter_shortcut = if enter_enabled {
        modal::ModalShortcut::enabled("Enter", enter_action)
    } else {
        modal::ModalShortcut::disabled("Enter", enter_action)
    };
    let train_navigation = if state.player_company.fleet.trains.is_empty() {
        modal::ModalShortcut::disabled("↑↓/JK", modal::ModalAction::Train)
    } else {
        modal::ModalShortcut::enabled("↑↓/JK", modal::ModalAction::Train)
    };
    let unassign_shortcut = if unassign_enabled {
        modal::ModalShortcut::enabled("U", modal::ModalAction::Unassign)
    } else {
        modal::ModalShortcut::disabled("U", modal::ModalAction::Unassign)
    };
    let footer = modal::shortcut_line(&[
        modal::ModalShortcut::enabled("Esc", modal::ModalAction::Cancel),
        train_navigation,
        enter_shortcut,
        unassign_shortcut,
    ]);
    let modal_areas = modal::render_shell(frame, area, &title, footer);
    let [context_area, table_area] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(4)]).areas(modal_areas.body);

    let context = if let Some(rejection) = &flow.rejection {
        vec![
            Line::styled("Assignment unavailable", theme::warning()),
            Line::styled(rejection.clone(), theme::error()),
        ]
    } else if state.player_company.fleet.trains.is_empty() {
        vec![
            Line::styled("No Trains", theme::title()),
            Line::styled(
                "Buy a Train before assigning fleet to this Service.",
                theme::secondary(),
            ),
        ]
    } else if let Some(service) = service {
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
        vec![
            Line::styled("Allocate fleet to this Passenger Service.", theme::title()),
            Line::styled(route, theme::primary_value()),
            Line::styled(
                "Enter assigns or reassigns. U removes the selected Train allocation.",
                theme::secondary(),
            ),
        ]
    } else {
        vec![Line::styled(
            "Passenger Service is no longer available.",
            theme::error(),
        )]
    };
    frame.render_widget(
        Paragraph::new(context)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        context_area,
    );

    let rows = state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| {
            let assignment = match state.player_company.fleet.assigned_service_id(train.id) {
                Some(service_id) if service_id == flow.service_id => "Current".to_owned(),
                Some(service_id) => format!("R{}", service_id.get()),
                None => "Unassigned".to_owned(),
            };
            let (status, location) = match &train.status {
                TrainStatus::Ready { at } => ("READY", station_label(state, *at)),
                TrainStatus::Travelling { .. } => ("TRAVELLING", "En route".to_owned()),
            };
            Row::new(vec![
                Cell::from(format!("T{:02}", train.id.get())),
                Cell::from(
                    train
                        .nickname
                        .as_ref()
                        .map(|nickname| nickname.as_str())
                        .unwrap_or("—"),
                ),
                Cell::from(status),
                Cell::from(location),
                Cell::from(assignment),
            ])
        })
        .collect::<Vec<_>>();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(18),
            Constraint::Length(12),
            Constraint::Min(18),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(["Train", "Name", "State", "Location", "Assignment"]).style(theme::table_header()),
    )
    .style(theme::panel())
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("› ")
    .highlight_spacing(HighlightSpacing::Always);
    let mut table_state = TableState::default();
    if !state.player_company.fleet.trains.is_empty() {
        table_state.select(Some(flow.selected_index));
    }
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

    use super::{ServiceTrainAssignmentAction, ServiceTrainAssignmentFlow};

    #[test]
    fn service_assignment_can_assign_and_unassign_a_train() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(10_000_000);
        let service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();

        let mut flow = ServiceTrainAssignmentFlow::start(&state, service_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceTrainAssignmentAction::Assign {
                train_id,
                service_id,
            }
        );

        assign_train_to_service(&mut state, train_id, service_id).unwrap();
        let mut flow = ServiceTrainAssignmentFlow::start(&state, service_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Char('u'), &state),
            ServiceTrainAssignmentAction::Unassign { train_id }
        );
    }

    #[test]
    fn service_assignment_reassigns_a_ready_train_from_another_service() {
        let mut state = create_new_game(42, "Alden Passenger", UtcSeconds::from_unix_seconds(0));
        state.player_company.funds = Money::from_cents(10_000_000);
        let first_service_id = create_service(
            &mut state,
            vec![RailStationId::new(1), RailStationId::new(2)],
        )
        .unwrap();
        let second_service_id = create_service(
            &mut state,
            vec![RailStationId::new(2), RailStationId::new(3)],
        )
        .unwrap();
        let train_id = purchase_train(&mut state, 0, RailStationId::new(1)).unwrap();
        assign_train_to_service(&mut state, train_id, first_service_id).unwrap();

        let mut flow = ServiceTrainAssignmentFlow::start(&state, second_service_id).unwrap();
        assert_eq!(
            flow.handle_key(KeyCode::Enter, &state),
            ServiceTrainAssignmentAction::Assign {
                train_id,
                service_id: second_service_id,
            }
        );
    }
}
