//! Financial presentation for the Player Company's Company view.
//!
//! This module reads the stored financial totals and the simulation's finite
//! recovery evaluation. It does not mutate the game state or authorise any
//! recovery action.

use std::fmt::Write;

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Cell, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    model::{GameState, JourneyId, JourneyReceipt, Money, RailStationId},
    sim::finance::{
        FinancialEvaluation, FinancialStatus, RecoveryJourney, RecoveryOption,
        evaluate_financial_recovery,
    },
    ui::theme,
};

const MAXIMUM_RECOVERY_OPTIONS_SHOWN: usize = 3;

/// Persistent receipt browsing state. A receipt is selected by Journey ID, not
/// table position, so a newly settled Journey cannot move the reader to a
/// different receipt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReceiptSelection {
    selected_journey_id: Option<JourneyId>,
    table_state: TableState,
    page_size: usize,
}

impl ReceiptSelection {
    /// Moves the retained-receipt selection without changing the saved game.
    pub fn handle_key(&mut self, key: KeyCode, state: &GameState) {
        self.synchronize(state);
        let receipt_count = state.financials.recent_journey_receipts.len();
        let Some(selected) = self.table_state.selected() else {
            return;
        };
        let page_size = self.page_size.max(1);
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(receipt_count.saturating_sub(1)),
            KeyCode::PageUp => selected.saturating_sub(page_size),
            KeyCode::PageDown => selected
                .saturating_add(page_size)
                .min(receipt_count.saturating_sub(1)),
            _ => selected,
        };
        self.select_index(state, next);
    }

    /// Returns whether a receipt can be inspected after reconciling arrivals.
    pub fn has_selection(&mut self, state: &GameState) -> bool {
        self.synchronize(state);
        self.selected_journey_id.is_some()
    }

    fn synchronize(&mut self, state: &GameState) {
        let receipts = &state.financials.recent_journey_receipts;
        let previous_index = self.table_state.selected().unwrap_or(0);
        let selected = self
            .selected_journey_id
            .and_then(|journey_id| {
                receipts
                    .iter()
                    .rev()
                    .position(|receipt| receipt.journey_id == journey_id)
            })
            .or_else(|| {
                (!receipts.is_empty())
                    .then_some(previous_index.min(receipts.len().saturating_sub(1)))
            });
        self.selected_journey_id = selected.and_then(|index| {
            receipts
                .iter()
                .rev()
                .nth(index)
                .map(|receipt| receipt.journey_id)
        });
        if selected.is_none() {
            *self.table_state.offset_mut() = 0;
        }
        self.table_state.select(selected);
    }

    fn select_index(&mut self, state: &GameState, index: usize) {
        let Some(receipt) = state
            .financials
            .recent_journey_receipts
            .iter()
            .rev()
            .nth(index)
        else {
            return;
        };
        self.selected_journey_id = Some(receipt.journey_id);
        self.table_state.select(Some(index));
    }

    fn selected_receipt<'a>(&mut self, state: &'a GameState) -> Option<&'a JourneyReceipt> {
        self.synchronize(state);
        self.selected_journey_id.and_then(|journey_id| {
            state
                .financials
                .recent_journey_receipts
                .iter()
                .find(|receipt| receipt.journey_id == journey_id)
        })
    }

    fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size.max(1);
    }
}

/// Renders the Company workspace as aligned Ratatui panels. Compact layouts
/// keep the financial totals beside a scrollable receipt table; full receipt
/// detail opens as a focused page when the inspector cannot fit.
pub fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    receipt_details_open: bool,
) {
    selection.synchronize(state);
    if receipt_details_open && !(area.width >= 100 && area.height >= 20) {
        render_compact_receipt_details(frame, area, state, selection);
        return;
    }
    if area.width >= 100 && area.height >= 20 {
        render_wide_dashboard(frame, area, state, selection, receipt_details_open);
    } else if area.width >= 76 && area.height >= 12 {
        render_compact_dashboard(frame, area, state, selection);
    } else {
        render_tiny_dashboard(frame, area, state);
    }
}

fn render_wide_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    receipt_details_open: bool,
) {
    let evaluation = evaluate_financial_recovery(state);
    let [summary_area, totals_area, history_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(8),
        Constraint::Fill(1),
    ])
    .spacing(1)
    .areas(area);
    let [cash_area, status_area] =
        Layout::horizontal([Constraint::Length(40), Constraint::Fill(1)])
            .spacing(1)
            .areas(summary_area);

    render_cash_panel(frame, cash_area, state);
    render_status_panel(frame, status_area, &evaluation);
    render_totals_table(frame, totals_area, state);

    let [receipts_area, recovery_area] =
        Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)])
            .spacing(1)
            .areas(history_area);
    render_receipts_table(frame, receipts_area, state, selection, true);
    if receipt_details_open {
        render_receipt_details(
            frame,
            recovery_area,
            selection.selected_receipt(state),
            true,
        );
    } else {
        render_recovery_panel(frame, recovery_area, state, &evaluation);
    }
}

fn render_compact_dashboard(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let evaluation = evaluate_financial_recovery(state);
    let block = panel_block("Financial overview", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [summary_area, status_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)])
            .spacing(1)
            .areas(inner);
    render_compact_summary(frame, summary_area, state, &evaluation);
    render_receipts_table(frame, status_area, state, selection, false);
}

fn render_tiny_dashboard(frame: &mut Frame, area: Rect, state: &GameState) {
    let evaluation = evaluate_financial_recovery(state);
    let block = panel_block("Financial overview", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status = evaluation
        .as_ref()
        .map_or("[?] STATUS UNAVAILABLE".to_owned(), |evaluation| {
            status_label(evaluation.status).to_owned()
        });
    let lines = vec![
        Line::from(vec![
            Span::styled("Funds ", theme::secondary()),
            Span::styled(format_money(state.player_company.funds), theme::title()),
            Span::raw("  "),
            Span::styled(
                status,
                status_style(evaluation.as_ref().ok().map(|e| e.status)),
            ),
        ]),
        financial_line(
            "Fleet value",
            format_cents(fleet_value_cents(state)),
            theme::title(),
        ),
        financial_line(
            "Revenue",
            format_money(state.financials.operating_revenue),
            theme::primary_value(),
        ),
        financial_line(
            "Access fees",
            format_money(state.financials.infrastructure_access_fees),
            theme::primary_value(),
        ),
        financial_line(
            "Fuel costs",
            format_money(state.financials.fuel_costs),
            theme::primary_value(),
        ),
        financial_line(
            "Operating result",
            format_signed_cents(operating_result_cents(state)),
            result_style(operating_result_cents(state)),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_cash_panel(frame: &mut Frame, area: Rect, state: &GameState) {
    let block = panel_block("Cash position", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            financial_line(
                "Company Funds",
                format_money(state.player_company.funds),
                theme::title(),
            ),
            financial_line(
                "Fleet value",
                format_cents(fleet_value_cents(state)),
                theme::primary_value(),
            ),
            Line::styled("Cash now; Fleet at original price.", theme::secondary()),
        ])
        .style(theme::panel())
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_status_panel(
    frame: &mut Frame,
    area: Rect,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let block = panel_block("Financial status", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = match evaluation {
        Ok(evaluation) => vec![
            Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            ),
            Line::styled(
                status_explanation(evaluation.status),
                theme::primary_value(),
            ),
        ],
        Err(error) => vec![
            Line::styled("[?] STATUS UNAVAILABLE", theme::error()),
            Line::styled(
                format!("Financial recovery evaluation unavailable: {error}"),
                theme::error(),
            ),
        ],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_totals_table(frame: &mut Frame, area: Rect, state: &GameState) {
    let result = operating_result_cents(state);
    let rows = vec![
        money_row(
            "Operating Revenue",
            format_money(state.financials.operating_revenue),
            theme::primary_value(),
        ),
        money_row(
            "Infrastructure Access Fees",
            format_money(state.financials.infrastructure_access_fees),
            theme::primary_value(),
        ),
        money_row(
            "Fuel Costs",
            format_money(state.financials.fuel_costs),
            theme::primary_value(),
        ),
        money_row(
            "Signed Operating Result",
            format_signed_cents(result),
            result_style(result),
        ),
    ];
    let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(18)])
        .block(panel_block("Operating totals · lifetime", false))
        .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_receipts_table(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
    wide: bool,
) {
    let receipts = &state.financials.recent_journey_receipts;
    let title = format!("Journey receipts · {} retained history", receipts.len());
    if receipts.is_empty() {
        frame.render_widget(
            Paragraph::new("No retained Journey receipts yet. Operating Revenue is credited when a Journey arrives.")
                .block(panel_block(&title, true))
                .style(theme::panel())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let visible_items = usize::from(area.height.saturating_sub(5)).max(1);
    selection.set_page_size(visible_items);
    let rows = receipts.iter().rev().map(|receipt| {
        let result = receipt_result_cents(
            receipt.revenue,
            receipt.infrastructure_access_fee,
            receipt.fuel_cost,
        );
        if wide {
            Row::new([
                Cell::from(format!("J{:02}", receipt.journey_id.get())),
                Cell::from(format_money(receipt.revenue)),
                Cell::from(format_money(receipt.infrastructure_access_fee)),
                Cell::from(format_money(receipt.fuel_cost)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        } else {
            Row::new([
                Cell::from(format!("J{}", receipt.journey_id.get())),
                Cell::from(format_money(receipt.revenue)),
                Cell::from(format_signed_cents(result)).style(result_style(result)),
            ])
        }
    });
    let (header, widths) = if wide {
        (
            Row::new(["Journey", "Revenue", "Access fees", "Fuel", "Signed result"]),
            vec![
                Constraint::Length(7),
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(13),
                Constraint::Length(15),
            ],
        )
    } else {
        (
            Row::new(["ID", "Revenue", "Result"]),
            vec![
                Constraint::Length(5),
                Constraint::Fill(1),
                Constraint::Length(12),
            ],
        )
    };
    let table = Table::new(rows, widths)
        .header(header.style(theme::table_header()).bottom_margin(1))
        .block(panel_block(&title, true))
        .row_highlight_style(theme::selected_row())
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut selection.table_state);
}

fn render_recovery_panel(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let block = panel_block("Recovery access", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = match evaluation {
        Ok(evaluation) => {
            let mut lines = vec![Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            )];
            match evaluation.status {
                FinancialStatus::Operating => lines.push(Line::styled(
                    "No recovery action needed.",
                    theme::secondary(),
                )),
                FinancialStatus::BankruptcyDeferred => lines.push(Line::styled(
                    "An active Journey may still settle revenue.",
                    theme::secondary(),
                )),
                FinancialStatus::Insolvent => {
                    lines.push(Line::styled(
                        "Concrete recovery options:",
                        theme::secondary(),
                    ));
                    lines.extend(
                        evaluation
                            .recovery_options
                            .iter()
                            .take(MAXIMUM_RECOVERY_OPTIONS_SHOWN)
                            .map(|option| {
                                Line::styled(
                                    format!("· {}", recovery_option_description(state, option)),
                                    theme::primary_value(),
                                )
                            }),
                    );
                }
                FinancialStatus::Bankruptcy => lines.push(Line::styled(
                    "No finite recovery option remains.",
                    theme::secondary(),
                )),
            }
            lines
        }
        Err(error) => vec![Line::styled(
            format!("Recovery unavailable: {error}"),
            theme::error(),
        )],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_compact_summary(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    evaluation: &Result<FinancialEvaluation, impl std::fmt::Display>,
) {
    let block = panel_block("At a glance", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![
        match evaluation {
            Ok(evaluation) => Line::styled(
                status_label(evaluation.status),
                status_style(Some(evaluation.status)),
            ),
            Err(error) => Line::styled(format!("[?] STATUS UNAVAILABLE: {error}"), theme::error()),
        },
        financial_line(
            "Company Funds",
            format_money(state.player_company.funds),
            theme::title(),
        ),
        financial_line(
            "Fleet value",
            format_cents(fleet_value_cents(state)),
            theme::primary_value(),
        ),
        Line::from(""),
        financial_line(
            "Revenue",
            format_money(state.financials.operating_revenue),
            theme::primary_value(),
        ),
        financial_line(
            "Access fees",
            format_money(state.financials.infrastructure_access_fees),
            theme::primary_value(),
        ),
        financial_line(
            "Fuel costs",
            format_money(state.financials.fuel_costs),
            theme::primary_value(),
        ),
        financial_line(
            "Operating result",
            format_signed_cents(operating_result_cents(state)),
            result_style(operating_result_cents(state)),
        ),
    ];
    if let Ok(evaluation) = evaluation
        && let Some(option) = evaluation.recovery_options.first()
    {
        lines.push(Line::styled("Recovery option", theme::secondary()));
        lines.push(Line::styled(
            compact_recovery_description(state, option),
            theme::primary_value(),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_receipt_details(
    frame: &mut Frame,
    area: Rect,
    receipt: Option<&JourneyReceipt>,
    compact: bool,
) {
    let title = if compact {
        "Journey receipt · retained history"
    } else {
        "Selected Journey receipt"
    };
    let block = panel_block(title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = match receipt {
        Some(receipt) => {
            let result = receipt_result_cents(
                receipt.revenue,
                receipt.infrastructure_access_fee,
                receipt.fuel_cost,
            );
            vec![
                Line::styled(
                    format!("Journey {} · retained receipt", receipt.journey_id.get()),
                    theme::title(),
                ),
                Line::from(""),
                financial_line(
                    "Operating Revenue",
                    format_money(receipt.revenue),
                    theme::primary_value(),
                ),
                financial_line(
                    "Access fees",
                    format_money(receipt.infrastructure_access_fee),
                    theme::primary_value(),
                ),
                financial_line(
                    "Fuel cost",
                    format_money(receipt.fuel_cost),
                    theme::primary_value(),
                ),
                financial_line(
                    "Signed profit",
                    format_signed_cents(result),
                    result_style(result),
                ),
                Line::from(""),
                Line::styled("Esc returns to retained receipts.", theme::secondary()),
            ]
        }
        None => vec![Line::styled(
            "The selected receipt is no longer retained.",
            theme::secondary(),
        )],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_compact_receipt_details(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    selection: &mut ReceiptSelection,
) {
    let receipt = selection.selected_receipt(state);
    render_receipt_details(frame, area, receipt, true);
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

fn financial_line(label: &str, value: String, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<20}"), theme::secondary()),
        Span::styled(value, value_style),
    ])
}

fn money_row(label: &str, value: String, style: Style) -> Row<'static> {
    Row::new([
        Cell::from(label.to_owned()),
        Cell::from(format!("{value:>18}")).style(style),
    ])
}

fn status_label(status: FinancialStatus) -> &'static str {
    match status {
        FinancialStatus::Operating => "[OK] OPERATING",
        FinancialStatus::Insolvent => "[!] INSOLVENCY",
        FinancialStatus::BankruptcyDeferred => "[~] BANKRUPTCY DEFERRED",
        FinancialStatus::Bankruptcy => "[X] BANKRUPTCY",
    }
}

fn status_explanation(status: FinancialStatus) -> &'static str {
    match status {
        FinancialStatus::Operating => "Company Funds can cover at least one available Journey.",
        FinancialStatus::Insolvent => {
            "Company Funds cannot cover a Journey; finite recovery remains."
        }
        FinancialStatus::BankruptcyDeferred => {
            "An active Journey may still settle Operating Revenue before Bankruptcy is considered."
        }
        FinancialStatus::Bankruptcy => {
            "No finite sell, retain, rebuy, and dispatch option can return the Player Company to operation."
        }
    }
}

fn status_style(status: Option<FinancialStatus>) -> Style {
    match status {
        Some(FinancialStatus::Operating) => theme::success().bold(),
        Some(FinancialStatus::Insolvent | FinancialStatus::BankruptcyDeferred) => {
            theme::warning().bold()
        }
        Some(FinancialStatus::Bankruptcy) | None => theme::error().bold(),
    }
}

fn result_style(result: i128) -> Style {
    if result < 0 {
        theme::warning().bold()
    } else {
        theme::success().bold()
    }
}

fn operating_result_cents(state: &GameState) -> i128 {
    receipt_result_cents(
        state.financials.operating_revenue,
        state.financials.infrastructure_access_fees,
        state.financials.fuel_costs,
    )
}

fn receipt_result_cents(revenue: Money, access_fees: Money, fuel_costs: Money) -> i128 {
    i128::from(revenue.cents()) - i128::from(access_fees.cents()) - i128::from(fuel_costs.cents())
}

fn compact_recovery_description(state: &GameState, option: &RecoveryOption) -> String {
    match option {
        RecoveryOption::CashOnly { journey } => format!(
            "Dispatch Train {} · {}",
            journey.train_id.get(),
            format_money(journey.operating_cost)
        ),
        RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            ..
        } => format!(
            "Keep Train {}; sell {} · +{}",
            retained_train_id.get(),
            train_ids_label(sold_train_ids),
            format_money(*resale_proceeds)
        ),
        RecoveryOption::SellAllAndRebuy {
            sold_train_ids,
            resale_proceeds,
            catalogue_index,
            ..
        } => {
            let name = state
                .rules
                .balance
                .diesel_catalogue()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            format!(
                "Sell {}; buy {name} · +{}",
                train_ids_label(sold_train_ids),
                format_money(*resale_proceeds)
            )
        }
    }
}

/// Renders the Player Company's funds, operating totals, receipts, and
/// financial recovery status.
pub fn render(state: &GameState) -> String {
    let mut output = String::new();
    let financials = &state.financials;

    writeln!(output, "{}", state.player_company.name).expect("writing to a String cannot fail");
    writeln!(
        output,
        "Company Funds: {}",
        format_money(state.player_company.funds)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "Fleet value: {} (original purchase prices)",
        format_cents(fleet_value_cents(state)),
    )
    .expect("writing to a String cannot fail");

    writeln!(output, "\nOperating totals:").expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Operating Revenue: {}",
        format_money(financials.operating_revenue),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Infrastructure Access Fee: {}",
        format_money(financials.infrastructure_access_fees),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Fuel Cost: {}",
        format_money(financials.fuel_costs)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        output,
        "  Journey Profitability total: {}",
        format_cents(
            i128::from(financials.operating_revenue.cents())
                - i128::from(financials.infrastructure_access_fees.cents())
                - i128::from(financials.fuel_costs.cents()),
        ),
    )
    .expect("writing to a String cannot fail");

    render_receipts(&mut output, state);
    render_financial_status(&mut output, state);
    output
}

fn render_receipts(output: &mut String, state: &GameState) {
    let receipts = &state.financials.recent_journey_receipts;
    writeln!(output, "\nLatest receipts:").expect("writing to a String cannot fail");
    if receipts.is_empty() {
        writeln!(output, "  No Journey receipts have settled yet.")
            .expect("writing to a String cannot fail");
        return;
    }

    for receipt in receipts.iter().rev() {
        let profitability = i128::from(receipt.revenue.cents())
            - i128::from(receipt.infrastructure_access_fee.cents())
            - i128::from(receipt.fuel_cost.cents());
        writeln!(
            output,
            "  Journey {} — Revenue: {}; Infrastructure Access Fee: {}; Fuel Cost: {}; Journey Profitability: {}",
            receipt.journey_id.get(),
            format_money(receipt.revenue),
            format_money(receipt.infrastructure_access_fee),
            format_money(receipt.fuel_cost),
            format_cents(profitability),
        )
        .expect("writing to a String cannot fail");
    }
}

fn render_financial_status(output: &mut String, state: &GameState) {
    writeln!(output, "\nFinancial status:").expect("writing to a String cannot fail");
    let evaluation = match evaluate_financial_recovery(state) {
        Ok(evaluation) => evaluation,
        Err(error) => {
            writeln!(
                output,
                "  Financial recovery evaluation unavailable: {error}"
            )
            .expect("writing to a String cannot fail");
            return;
        }
    };

    match evaluation.status {
        FinancialStatus::Operating => writeln!(
            output,
            "  Operating: Company Funds can cover at least one available Journey.",
        )
        .expect("writing to a String cannot fail"),
        FinancialStatus::BankruptcyDeferred => writeln!(
            output,
            "  Insolvency assessment deferred: an active Journey may still settle Operating Revenue before Bankruptcy is considered.",
        )
        .expect("writing to a String cannot fail"),
        FinancialStatus::Insolvent => {
            writeln!(
                output,
                "  INSOLVENCY: Company Funds cannot currently cover an available Journey, but a finite recovery option remains.",
            )
            .expect("writing to a String cannot fail");
            render_recovery_options(output, state, &evaluation.recovery_options);
        }
        FinancialStatus::Bankruptcy => writeln!(
            output,
            "  BANKRUPTCY: no finite sell, retain, rebuy, and dispatch option can return the Player Company to operation.",
        )
        .expect("writing to a String cannot fail"),
    }
}

fn render_recovery_options(output: &mut String, state: &GameState, options: &[RecoveryOption]) {
    writeln!(output, "  Recovery options:").expect("writing to a String cannot fail");
    for option in options.iter().take(MAXIMUM_RECOVERY_OPTIONS_SHOWN) {
        writeln!(
            output,
            "    - {}",
            recovery_option_description(state, option)
        )
        .expect("writing to a String cannot fail");
    }
    if options.len() > MAXIMUM_RECOVERY_OPTIONS_SHOWN {
        writeln!(
            output,
            "    Showing {} of {} finite recovery options.",
            MAXIMUM_RECOVERY_OPTIONS_SHOWN,
            options.len(),
        )
        .expect("writing to a String cannot fail");
    }
}

fn recovery_option_description(state: &GameState, option: &RecoveryOption) -> String {
    match option {
        RecoveryOption::CashOnly { journey } => format!(
            "Dispatch Train {} from {} to {} for {}.",
            journey.train_id.get(),
            station_label(state, journey.origin_station_id),
            station_label(state, journey.destination_station_id),
            format_money(journey.operating_cost),
        ),
        RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            journey,
        } => format!(
            "Keep Train {}; sell {} for {}; then {}",
            retained_train_id.get(),
            train_ids_label(sold_train_ids),
            format_money(*resale_proceeds),
            journey_description(state, journey),
        ),
        RecoveryOption::SellAllAndRebuy {
            sold_train_ids,
            resale_proceeds,
            catalogue_index,
            delivery_station_id,
            journey,
        } => {
            let catalogue_name = state
                .rules
                .balance
                .diesel_catalogue()
                .get(*catalogue_index)
                .map_or("catalogue Train", |train| train.name());
            format!(
                "Sell {} for {}; buy {catalogue_name} at {}; then {}",
                train_ids_label(sold_train_ids),
                format_money(*resale_proceeds),
                station_label(state, *delivery_station_id),
                journey_description(state, journey),
            )
        }
    }
}

fn journey_description(state: &GameState, journey: &RecoveryJourney) -> String {
    format!(
        "dispatch Train {} from {} to {} for {}.",
        journey.train_id.get(),
        station_label(state, journey.origin_station_id),
        station_label(state, journey.destination_station_id),
        format_money(journey.operating_cost),
    )
}

fn train_ids_label(train_ids: &[crate::model::TrainId]) -> String {
    match train_ids {
        [] => "no Trains".into(),
        [train_id] => format!("Train {}", train_id.get()),
        _ => format!(
            "Trains {}",
            train_ids
                .iter()
                .map(|train_id| train_id.get().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn fleet_value_cents(state: &GameState) -> i128 {
    state
        .player_company
        .fleet
        .trains
        .iter()
        .map(|train| i128::from(train.original_purchase_price.cents()))
        .sum()
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

fn format_money(money: Money) -> String {
    format_cents(i128::from(money.cents()))
}

fn format_signed_cents(cents: i128) -> String {
    if cents > 0 {
        format!("+{}", format_cents(cents))
    } else {
        format_cents(cents)
    }
}

fn format_cents(cents: i128) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    let whole = (cents / 100).to_string();
    let grouped_whole = whole
        .chars()
        .rev()
        .enumerate()
        .fold(String::new(), |mut output, (index, digit)| {
            if index != 0 && index % 3 == 0 {
                output.push(',');
            }
            output.push(digit);
            output
        })
        .chars()
        .rev()
        .collect::<String>();
    format!("{sign}${grouped_whole}.{:02}", cents % 100)
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{Money, RailStationId, UtcSeconds},
        sim::{
            fleet::purchase_train, journeys::dispatch_journey, services::find_or_create_service,
            time::advance_time, world::create_new_game,
        },
    };

    use super::render;

    const STARTED_AT: UtcSeconds = UtcSeconds::from_unix_seconds(1_000);
    const ORIGIN: RailStationId = RailStationId::new(1);
    const DESTINATION: RailStationId = RailStationId::new(2);

    #[test]
    fn company_view_shows_totals_fleet_value_and_latest_receipt() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        let train_id = purchase_train(&mut state, 0, ORIGIN).unwrap();
        let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION).unwrap();
        dispatch_journey(&mut state, train_id, service_id, STARTED_AT).unwrap();
        let arrives_at = state.active_journeys[0].arrives_at;
        advance_time(&mut state, arrives_at).unwrap();

        let rendered = render(&state);

        assert!(rendered.contains("Company Funds: $"));
        assert!(rendered.contains("Fleet value: $3,000.00"));
        assert!(rendered.contains("Operating Revenue: $"));
        assert!(rendered.contains("Infrastructure Access Fee: $"));
        assert!(rendered.contains("Fuel Cost: $"));
        assert!(rendered.contains("Journey Profitability total: $"));
        assert!(rendered.contains("Latest receipts:"));
        assert!(rendered.contains("Journey 1 — Revenue:"));
    }

    #[test]
    fn company_view_explains_insolvency_and_lists_recovery_options() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::from_cents(1_000_000);
        purchase_train(&mut state, 0, ORIGIN).unwrap();
        purchase_train(&mut state, 0, ORIGIN).unwrap();
        state.player_company.funds = Money::ZERO;

        let rendered = render(&state);

        assert!(rendered.contains("INSOLVENCY:"));
        assert!(rendered.contains("Recovery options:"));
        assert!(rendered.contains("Keep Train"));
    }

    #[test]
    fn company_view_shows_bankruptcy_when_no_option_exists() {
        let mut state = create_new_game(42, "Alden Passenger", STARTED_AT);
        state.player_company.funds = Money::ZERO;

        let rendered = render(&state);

        assert!(rendered.contains("BANKRUPTCY:"));
        assert!(!rendered.contains("Recovery options:"));
    }
}
