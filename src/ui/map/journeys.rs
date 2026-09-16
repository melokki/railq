//! Journey-board presentation for the Map workspace.
//!
//! This module owns active-Journey selection and the departure-board/inspector
//! rendering. Journey simulation remains in the domain/simulation layer.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Cell, LineGauge, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{GameState, Journey, JourneyId, TrainId, UtcSeconds},
    ui::theme,
};

use super::shared::{format_duration, journey_progress_percent, remaining_seconds};
use super::network::{labelled_line, panel_block, station_name};
use super::operational::journey_next_stop_station_id;

/// Presentation-only selection for the active departure board.
///
/// A selected Journey is identified by its stable ID. Its Train ID is retained
/// separately, so reconciliation can remove the arrived Journey while the
/// Fleet still opens on that now-READY Train.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JourneySelection {
    selected_journey_id: Option<JourneyId>,
    selected_train_id: Option<TrainId>,
    table_state: TableState,
    page_size: usize,
}

impl JourneySelection {
    /// Moves through active Journeys without changing the simulation.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let last = state.active_journeys.len().saturating_sub(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected.saturating_add(1).min(last),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected.saturating_add(page_size).min(last),
            _ => selected,
        };
        self.select_index(state, next);
    }

    /// Returns the selected active Journey after reconciling its stable ID.
    pub fn selected_journey_id(&mut self, state: &GameState) -> Option<JourneyId> {
        self.synchronize(state);
        self.selected_journey_id
    }

    /// Returns the Train associated with the last explicitly selected Journey.
    /// It intentionally survives an arrival so Fleet selection remains useful.
    pub fn selected_train_id(&mut self, state: &GameState) -> Option<TrainId> {
        self.synchronize(state);
        self.selected_train_id
    }

    pub(super) fn synchronize(&mut self, state: &GameState) {
        let journeys = &state.active_journeys;
        let previous_index = self.table_state.selected().unwrap_or(0);
        let retained = self
            .selected_journey_id
            .and_then(|journey_id| journeys.iter().position(|journey| journey.id == journey_id));
        let fallback = if journeys.is_empty() {
            None
        } else {
            Some(previous_index.min(journeys.len() - 1))
        };
        let selected = retained.or(fallback);
        if let Some(index) = selected {
            self.selected_journey_id = Some(journeys[index].id);
            if self.selected_train_id.is_none() {
                self.selected_train_id = Some(journeys[index].train_id);
            }
        } else {
            self.selected_journey_id = None;
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(journey) = state.active_journeys.get(index) else {
            return;
        };
        self.selected_journey_id = Some(journey.id);
        self.selected_train_id = Some(journey.train_id);
        self.table_state.select(Some(index));
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }

    fn keep_compact_selection_visible(&mut self, visible_items: usize) {
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let visible_items = visible_items.max(1);
        let offset = self.table_state.offset();
        if selected < offset {
            *self.table_state.offset_mut() = selected;
        } else if selected >= offset.saturating_add(visible_items) {
            *self.table_state.offset_mut() =
                selected.saturating_add(1).saturating_sub(visible_items);
        }
    }
}

pub(super) fn render_journey_workspace(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
) {
    let [board_area, inspector_area] =
        Layout::horizontal([Constraint::Min(58), Constraint::Length(32)])
            .spacing(1)
            .areas(area);
    render_journey_table(frame, board_area, state, now, selection, true);
    render_journey_inspector(frame, inspector_area, state, now, selection, false);
}

fn render_journey_table(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
    focused: bool,
) {
    let visible_items = usize::from(area.height.saturating_sub(4)).max(1);
    selection.set_page_size(visible_items);
    let rows = state
        .active_journeys
        .iter()
        .map(|journey| {
            Row::new([
                Cell::from(format!("Train {:02}", journey.train_id.get())),
                Cell::from(station_name(state, journey.origin_station_id)),
                Cell::from(
                    journey_next_stop_station_id(state, journey)
                        .map(|station_id| station_name(state, station_id))
                        .unwrap_or_else(|| "unknown".to_string()),
                ),
                Cell::from(format!(
                    "in {}",
                    format_duration(remaining_seconds(journey, now))
                )),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Percentage(31),
            Constraint::Percentage(31),
            Constraint::Length(11),
        ],
    )
    .header(
        Row::new(["Train", "Origin", "Next stop", "ETA"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .block(panel_block("Departure Board · active Journeys", focused))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol(theme::SELECTION_MARKER)
    .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

pub(super) fn render_compact_journey_board(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
) {
    if state.active_journeys.is_empty() {
        render_empty_journey_board(frame, area, true);
        return;
    }
    let visible_items = usize::from(area.height.saturating_sub(2) / 3).max(1);
    selection.set_page_size(visible_items);
    selection.keep_compact_selection_visible(visible_items);
    let offset = selection.table_state.offset();
    let selected = selection.table_state.selected();
    let lines = state
        .active_journeys
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_items)
        .flat_map(|(index, journey)| {
            compact_journey_lines(state, journey, now, selected == Some(index))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Departure Board · active Journeys", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

pub(super) fn render_journey_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    now: UtcSeconds,
    selection: &mut JourneySelection,
    focused: bool,
) {
    if state.active_journeys.is_empty() {
        render_empty_journey_board(frame, area, focused);
        return;
    }
    let block = panel_block("Selected Journey", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let selected = selection.selected_journey_id(state).and_then(|journey_id| {
        state
            .active_journeys
            .iter()
            .find(|journey| journey.id == journey_id)
    });
    let Some(journey) = selected else {
        frame.render_widget(
            Paragraph::new("No active Journey selected.").style(theme::panel()),
            inner,
        );
        return;
    };
    let percent = u16::try_from(journey_progress_percent(journey, now)).unwrap_or(100);
    let [details_area, gauge_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let lines = vec![
        Line::styled(
            format!("Train {:02}", journey.train_id.get()),
            theme::title(),
        ),
        labelled_line(
            "Route",
            &format!(
                "{} → {}",
                station_name(state, journey.origin_station_id),
                station_name(state, journey.destination_station_id)
            ),
        ),
        labelled_line(
            "Next stop",
            &journey_next_stop_station_id(state, journey)
                .map(|station_id| station_name(state, station_id).to_owned())
                .unwrap_or_else(|| "unknown".into()),
        ),
        labelled_line(
            "ETA",
            &format!("in {}", format_duration(remaining_seconds(journey, now))),
        ),
        labelled_line("On board", &journey.onboard_passengers().to_string()),
        labelled_line("Leg progress", &format!("{percent}%")),
        Line::from(""),
        Line::styled(
            "Map marker follows the current Service leg.",
            theme::secondary(),
        ),
        Line::styled("Real-time Journeys continue", theme::hint()),
        Line::styled("after exit.", theme::hint()),
        Line::from(""),
        Line::styled(
            if focused {
                "Esc · departure board    Tab · Rail Stations"
            } else {
                "Enter · focused details    Tab · next Map panel"
            },
            theme::hint(),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        details_area,
    );
    frame.render_widget(
        LineGauge::default()
            .ratio(f64::from(percent) / 100.0)
            .label(format!("{percent}%"))
            .filled_style(Style::default().fg(theme::ACCENT).bg(theme::PANEL))
            .unfilled_style(Style::default().fg(theme::SECONDARY).bg(theme::PANEL)),
        gauge_area,
    );
}

fn render_empty_journey_board(frame: &mut Frame, area: Rect, focused: bool) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("No active Journeys", theme::title()),
            Line::styled(
                "Press D anywhere on the Map to choose from all READY Trains.",
                theme::secondary(),
            ),
            Line::styled("Journeys continue in real time after exit.", theme::hint()),
        ])
        .block(panel_block("Departure Board · active Journeys", focused))
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn compact_journey_lines(
    state: &GameState,
    journey: &Journey,
    now: UtcSeconds,
    selected: bool,
) -> [Line<'static>; 3] {
    let marker = if selected { ">" } else { " " };
    let row_style = selected
        .then(theme::selected_row)
        .unwrap_or_else(theme::panel);
    [
        Line::styled(
            format!(
                "{marker} Train {:02}  ETA in {}",
                journey.train_id.get(),
                format_duration(remaining_seconds(journey, now))
            ),
            row_style.add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(
                "  {} → {}",
                station_name(state, journey.origin_station_id),
                station_name(state, journey.destination_station_id)
            ),
            row_style,
        ),
        Line::styled(
            format!("  {}% to next stop", journey_progress_percent(journey, now)),
            row_style,
        ),
    ]
}
