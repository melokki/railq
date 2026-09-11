//! Keyboard state and text presentation for buying catalogue Trains.
//!
//! A proposed purchase remains presentation-only until the Player Company
//! explicitly confirms it. The application boundary then revalidates the
//! catalogue entry, selected Rail Station, and Company Funds before saving.

use std::fmt::Write;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    balance::DieselTrainCatalogueRecord,
    model::{GameState, Money, RailLine, RailStationId},
    ui::theme,
};

/// Persistent catalogue focus. Catalogue records are saved with a game, so an
/// index is stable for the duration of this presentation-only purchase path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogueSelection {
    selected: usize,
    table_state: TableState,
    page_size: usize,
}

impl CatalogueSelection {
    /// Returns the currently focused catalogue record, if one remains available.
    pub fn selected_catalogue_index(&mut self, state: &GameState) -> Option<usize> {
        self.synchronize(state);
        (!state.rules.balance.diesel_catalogue().is_empty()).then_some(self.selected)
    }

    /// Moves the focused catalogue record without changing the game state.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let catalogue_len = state.rules.balance.diesel_catalogue().len();
        if catalogue_len == 0 {
            return;
        }
        self.selected = match key {
            KeyCode::Up | KeyCode::Char('k' | 'K') => self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j' | 'J') => self
                .selected
                .saturating_add(1)
                .min(catalogue_len.saturating_sub(1)),
            KeyCode::PageUp => self.selected.saturating_sub(self.page_size.max(1)),
            KeyCode::PageDown => self
                .selected
                .saturating_add(self.page_size.max(1))
                .min(catalogue_len.saturating_sub(1)),
            _ => self.selected,
        };
        self.table_state.select(Some(self.selected));
    }

    fn synchronize(&mut self, state: &GameState) {
        let catalogue_len = state.rules.balance.diesel_catalogue().len();
        if catalogue_len == 0 {
            self.selected = 0;
            self.table_state.select(None);
            *self.table_state.offset_mut() = 0;
        } else {
            self.selected = self.selected.min(catalogue_len.saturating_sub(1));
            self.table_state.select(Some(self.selected));
        }
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
}

/// The result of handling a key within the Buy Trains flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketFlowAction {
    /// The player is still reviewing a proposed purchase.
    Continue,
    /// The player abandoned the proposed purchase.
    Cancel,
    /// The application boundary must revalidate and purchase the Train.
    Confirm {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MarketStep {
    SelectDelivery {
        catalogue_index: usize,
        selected: usize,
    },
    Confirm {
        catalogue_index: usize,
        delivery_station_id: RailStationId,
    },
}

/// Presentation state for one uncommitted catalogue Train purchase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketFlow {
    step: MarketStep,
    rejection: Option<String>,
}

impl MarketFlow {
    /// Starts delivery selection for a focused catalogue Train without changing
    /// the Fleet or Company Funds.
    pub fn start(state: &GameState, catalogue_index: usize) -> Result<Self, &'static str> {
        if state.rules.balance.diesel_catalogue().is_empty() {
            return Err("No diesel Train is available in the catalogue.");
        }
        if state
            .rules
            .balance
            .diesel_catalogue()
            .get(catalogue_index)
            .is_none()
        {
            return Err("The selected catalogue Train is no longer available.");
        }
        if delivery_station_ids(state).is_empty() {
            return Err("No connected Rail Station is available for delivery.");
        }
        Ok(Self {
            step: MarketStep::SelectDelivery {
                catalogue_index,
                selected: 0,
            },
            rejection: None,
        })
    }

    /// Applies one keyboard command without modifying the supplied game state.
    pub fn handle_key(&mut self, key: KeyEvent, state: &GameState) -> MarketFlowAction {
        if matches!(key.code, KeyCode::Esc) {
            return MarketFlowAction::Cancel;
        }

        match &mut self.step {
            MarketStep::SelectDelivery {
                catalogue_index,
                selected,
            } => {
                let stations = delivery_station_ids(state);
                if stations.is_empty() {
                    self.rejection =
                        Some("No connected Rail Station is available for delivery.".into());
                    return MarketFlowAction::Continue;
                }
                move_selection(selected, stations.len(), key.code);
                if matches!(key.code, KeyCode::Enter) {
                    self.step = MarketStep::Confirm {
                        catalogue_index: *catalogue_index,
                        delivery_station_id: stations[*selected],
                    };
                    self.rejection = None;
                }
                MarketFlowAction::Continue
            }
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } if matches!(key.code, KeyCode::Enter) => MarketFlowAction::Confirm {
                catalogue_index: *catalogue_index,
                delivery_station_id: *delivery_station_id,
            },
            MarketStep::Confirm { .. } => MarketFlowAction::Continue,
        }
    }

    /// Records an application-boundary rejection while keeping the proposal visible.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.rejection = Some(error.into());
    }

    /// Renders the proposed purchase and its current-step instructions.
    pub fn render(&self, state: &GameState) -> String {
        let selected_catalogue_index = match &self.step {
            MarketStep::SelectDelivery {
                catalogue_index, ..
            }
            | MarketStep::Confirm {
                catalogue_index, ..
            } => *catalogue_index,
        };
        let mut output = render_selected(state, selected_catalogue_index);

        match &self.step {
            MarketStep::SelectDelivery {
                catalogue_index: _,
                selected,
            } => {
                writeln!(
                    output,
                    "Select a delivery Rail Station (Up/Down, Enter; Esc cancels):"
                )
                .expect("writing to a String cannot fail");
                for (index, station_id) in delivery_station_ids(state).iter().enumerate() {
                    let marker = if index == *selected { '>' } else { ' ' };
                    writeln!(output, " {marker} {}", station_label(state, *station_id))
                        .expect("writing to a String cannot fail");
                }
            }
            MarketStep::Confirm {
                catalogue_index,
                delivery_station_id,
            } => {
                let train = state.rules.balance.diesel_catalogue().get(*catalogue_index);
                if let Some(train) = train {
                    writeln!(
                        output,
                        "Deliver {} to {} at no delivery fee.",
                        train.name(),
                        station_label(state, *delivery_station_id)
                    )
                    .expect("writing to a String cannot fail");
                    if low_reserve(state, train) {
                        writeln!(
                            output,
                            "LOW RESERVE WARNING: Company Funds after purchase cannot cover the sample trip's departure cost."
                        )
                        .expect("writing to a String cannot fail");
                    }
                }
                writeln!(
                    output,
                    "Enter confirms purchase (revalidated); Esc cancels."
                )
                .expect("writing to a String cannot fail");
            }
        }
        if let Some(rejection) = &self.rejection {
            writeln!(output, "Purchase rejected: {rejection}")
                .expect("writing to a String cannot fail");
        }
        output
    }
}

/// Renders the diesel Train catalogue before a purchase is started.
pub fn render(state: &GameState) -> String {
    let mut output = String::from("Buy Trains\n");
    for (index, train) in state.rules.balance.diesel_catalogue().iter().enumerate() {
        render_catalogue_train(&mut output, state, index, train);
    }
    if let Some(train) = state.rules.balance.diesel_catalogue().first() {
        writeln!(output, "Selected Train: {}", train.name())
            .expect("writing to a String cannot fail");
        render_purchase_implications(&mut output, state, train);
    } else {
        writeln!(output, "No diesel Train is available in the catalogue.")
            .expect("writing to a String cannot fail");
    }
    writeln!(
        output,
        "Enter starts a purchase; Esc is available after selection starts."
    )
    .expect("writing to a String cannot fail");
    output
}

/// Renders the selectable catalogue and focused model inspector.
///
/// The compact table deliberately retains price, capacity, speed and fuel
/// cost, so both catalogue models remain comparable at 80×24.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut CatalogueSelection,
) {
    selection.synchronize(state);
    let catalogue = state.rules.balance.diesel_catalogue();
    if catalogue.is_empty() {
        frame.render_widget(
            Paragraph::new("No diesel Train is available in the catalogue.")
                .block(panel_block("Buy Trains · catalogue", true))
                .style(theme::panel()),
            area,
        );
        return;
    }

    let wide = area.width >= 96 && area.height >= 14;
    let [table_area, inspector_area] = if wide {
        Layout::horizontal([Constraint::Min(54), Constraint::Length(34)])
            .spacing(1)
            .areas(area)
    } else {
        Layout::vertical([Constraint::Length(8), Constraint::Min(6)]).areas(area)
    };
    selection.set_page_size(usize::from(table_area.height.saturating_sub(4)).max(1));
    selection.synchronize(state);

    let rows = catalogue
        .iter()
        .map(|train| {
            Row::new([
                Cell::from(train.name().to_owned()),
                Cell::from(format_money(train.purchase_price())),
                Cell::from(format!("{} pax", train.passenger_capacity().passengers())),
                Cell::from(format_speed_kmh(train)),
                Cell::from(format!(
                    "{}/km",
                    format_money_per_kilometre(
                        train.fuel_cost_per_kilometre().cents_per_kilometre()
                    )
                )),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        if wide {
            vec![
                Constraint::Percentage(25),
                Constraint::Percentage(22),
                Constraint::Percentage(16),
                Constraint::Percentage(18),
                Constraint::Percentage(19),
            ]
        } else {
            vec![
                Constraint::Percentage(23),
                Constraint::Percentage(22),
                Constraint::Percentage(14),
                Constraint::Percentage(19),
                Constraint::Percentage(22),
            ]
        },
    )
    .header(
        Row::new(["Model", "Price", "Capacity", "km/h", "Fuel / km"])
            .style(theme::table_header())
            .bottom_margin(1),
    )
    .block(panel_block("Buy Trains · catalogue", true))
    .row_highlight_style(theme::selected_row())
    .highlight_symbol("> ")
    .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, table_area, &mut selection.table_state);

    let selected_train = selection
        .selected_catalogue_index(state)
        .and_then(|index| catalogue.get(index));
    render_catalogue_inspector(frame, inspector_area, state, selected_train, wide);
}

fn render_catalogue_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    train: Option<&DieselTrainCatalogueRecord>,
    wide: bool,
) {
    let Some(train) = train else {
        return;
    };
    let mut lines = vec![
        Line::styled(train.name(), theme::focused_title()),
        Line::from(format!("Price: {}", format_money(train.purchase_price()))),
        Line::from(format!(
            "Capacity: {} passengers",
            train.passenger_capacity().passengers()
        )),
        Line::from(format!("Speed: {}", format_speed_kmh(train))),
        Line::from(format!(
            "Fuel cost: {}/km",
            format_money_per_kilometre(train.fuel_cost_per_kilometre().cents_per_kilometre())
        )),
    ];
    if wide {
        if let Some(sample) = sample_trip(state, train) {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                format!("Sample Rail Line · {}", sample.route),
                theme::secondary(),
            ));
            lines.push(Line::from(format!(
                "{} · departure cost {}",
                sample.distance,
                format_money(sample.departure_cost)
            )));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(
        "Enter · choose delivery Rail Station",
        theme::hint(),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Selected model", true))
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(theme::THIN_BORDERS)
        .border_style(if focused {
            theme::focused_border()
        } else {
            theme::border()
        })
        .title(title)
        .title_style(if focused {
            theme::focused_title()
        } else {
            theme::title()
        })
        .style(theme::panel())
}

fn render_selected(state: &GameState, selected_catalogue_index: usize) -> String {
    let mut output = String::from("Buy Trains\n");
    for (index, train) in state.rules.balance.diesel_catalogue().iter().enumerate() {
        render_catalogue_train(&mut output, state, index, train);
    }
    if let Some(train) = state
        .rules
        .balance
        .diesel_catalogue()
        .get(selected_catalogue_index)
    {
        writeln!(output, "Selected Train: {}", train.name())
            .expect("writing to a String cannot fail");
        render_purchase_implications(&mut output, state, train);
    }
    output
}

fn render_catalogue_train(
    output: &mut String,
    state: &GameState,
    index: usize,
    train: &DieselTrainCatalogueRecord,
) {
    writeln!(output, "{}: {}", index + 1, train.name()).expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Capacity: {} passengers | Speed: {} | Fuel Cost: {}/km",
        train.passenger_capacity().passengers(),
        format_speed_kmh(train),
        format_money_per_kilometre(train.fuel_cost_per_kilometre().cents_per_kilometre()),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Purchase price: {}",
        format_money(train.purchase_price())
    )
    .expect("writing to a String cannot fail");
    if let Some(sample) = sample_trip(state, train) {
        writeln!(
            output,
            "  Sample trip ({}, {}): Infrastructure Access Fee {} + Fuel Cost {} = {} departure cost",
            sample.route,
            sample.distance,
            format_money(sample.access_fee),
            format_money(sample.fuel_cost),
            format_money(sample.departure_cost),
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_purchase_implications(
    output: &mut String,
    state: &GameState,
    train: &DieselTrainCatalogueRecord,
) {
    match state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    {
        Ok(remaining) if remaining >= Money::ZERO => writeln!(
            output,
            "Company Funds after purchase: {}",
            format_money(remaining)
        )
        .expect("writing to a String cannot fail"),
        Ok(_) => writeln!(
            output,
            "Company Funds after purchase: insufficient Company Funds (currently {})",
            format_money(state.player_company.funds)
        )
        .expect("writing to a String cannot fail"),
        Err(_) => writeln!(output, "Company Funds after purchase: unavailable")
            .expect("writing to a String cannot fail"),
    }
}

struct SampleTrip {
    route: String,
    distance: String,
    access_fee: Money,
    fuel_cost: Money,
    departure_cost: Money,
}

fn sample_trip(state: &GameState, train: &DieselTrainCatalogueRecord) -> Option<SampleTrip> {
    let line = state
        .region
        .rail_authority
        .rail_network
        .rail_lines
        .iter()
        .min_by_key(|line| line.distance.metres())?;
    let access_fee = state
        .rules
        .balance
        .access_fee_per_train_kilometre()
        .checked_charge(line.distance)
        .ok()?;
    let fuel_cost = train
        .fuel_cost_per_kilometre()
        .checked_charge(line.distance)
        .ok()?;
    let departure_cost = access_fee.checked_add(fuel_cost).ok()?;
    Some(SampleTrip {
        route: format!(
            "{} -> {}",
            station_label(state, line.first_station_id),
            station_label(state, line.second_station_id)
        ),
        distance: format_distance(line),
        access_fee,
        fuel_cost,
        departure_cost,
    })
}

fn low_reserve(state: &GameState, train: &DieselTrainCatalogueRecord) -> bool {
    let Ok(remaining) = state
        .player_company
        .funds
        .checked_sub(train.purchase_price())
    else {
        return true;
    };
    remaining < Money::ZERO
        || sample_trip(state, train).is_some_and(|sample| remaining < sample.departure_cost)
}

fn delivery_station_ids(state: &GameState) -> Vec<RailStationId> {
    state
        .region
        .rail_authority
        .rail_network
        .rail_stations
        .iter()
        .map(|station| station.id)
        .collect()
}

fn move_selection(selected: &mut usize, length: usize, key: KeyCode) {
    match key {
        KeyCode::Up | KeyCode::Char('k') if *selected > 0 => *selected -= 1,
        KeyCode::Down | KeyCode::Char('j') if *selected + 1 < length => *selected += 1,
        _ => {}
    }
}

fn station_label(state: &GameState, station_id: RailStationId) -> &str {
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

fn format_distance(line: &RailLine) -> String {
    let metres = line.distance.metres();
    format!("{}.{:03} km", metres / 1_000, metres % 1_000)
}

fn format_money_per_kilometre(cents: u64) -> String {
    format_money(Money::from_cents(i64::try_from(cents).unwrap_or(i64::MAX)))
}

fn format_speed_kmh(train: &DieselTrainCatalogueRecord) -> String {
    let tenths = u128::from(train.speed().metres_per_second()).saturating_mul(36);
    format!("{}.{:01} km/h", tenths / 10, tenths % 10)
}

fn format_money(money: Money) -> String {
    let cents = i128::from(money.cents());
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.abs();
    format!(
        "{sign}${}.{:02}",
        format_grouped_integer(cents / 100),
        cents % 100
    )
}

fn format_grouped_integer(value: i128) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::world::create_new_game,
    };

    use super::{MarketFlow, MarketFlowAction, render};

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn catalogue_shows_both_diesel_trains_with_price_stats_and_sample_cost() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let rendered = render(&state);

        for train in state.rules.balance.diesel_catalogue() {
            assert!(rendered.contains(train.name()));
            assert!(rendered.contains(&format!(
                "Capacity: {} passengers",
                train.passenger_capacity().passengers()
            )));
        }
        assert!(rendered.contains("Speed:"));
        assert!(rendered.contains("Fuel Cost:"));
        assert!(rendered.contains("Purchase price:"));
        assert!(rendered.contains("Sample trip"));
        assert!(rendered.contains("Infrastructure Access Fee"));
    }

    #[test]
    fn flow_selects_a_connected_delivery_station_and_requires_confirmation() {
        let state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let mut flow = MarketFlow::start(&state, 1).unwrap();

        let delivery_selection = flow.render(&state);
        for station in &state.region.rail_authority.rail_network.rail_stations {
            let settlement = state
                .region
                .settlements
                .iter()
                .find(|settlement| settlement.id == station.settlement_id)
                .unwrap();
            assert!(delivery_selection.contains(&settlement.name));
        }

        flow.handle_key(key(KeyCode::Down), &state);
        flow.handle_key(key(KeyCode::Enter), &state);
        assert_eq!(
            flow.handle_key(key(KeyCode::Enter), &state),
            MarketFlowAction::Confirm {
                catalogue_index: 1,
                delivery_station_id: RailStationId::new(2),
            }
        );
    }

    #[test]
    fn confirmation_warns_when_purchase_leaves_no_sample_trip_reserve() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::from_cents(500_000);
        let mut flow = MarketFlow::start(&state, 1).unwrap();

        flow.handle_key(key(KeyCode::Enter), &state);
        flow.handle_key(key(KeyCode::Enter), &state);

        let rendered = flow.render(&state);
        assert!(rendered.contains("Company Funds after purchase: $0.00"));
        assert!(rendered.contains("LOW RESERVE WARNING"));
        assert!(rendered.contains("Enter confirms purchase"));
    }
}
