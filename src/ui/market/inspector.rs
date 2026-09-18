//! Selected rolling-stock model presentation for the Market workspace.
//!
//! The catalogue is for comparison; this inspector is for the purchase
//! decision. It leads with acquisition and operating consequences while
//! retaining the railway registration details that give each model identity.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    catalog::TrainModel,
    model::GameState,
    ui::{
        components::{labelled_line, labelled_line_styled, section_heading},
        theme,
    },
};

use super::{
    CatalogueOwnership, catalogue_ownership, delivery_station_ids, format_money,
    format_money_per_kilometre, format_speed_kmh, funds_after_purchase_display,
    horizontal_inset, low_reserve, reserve_after_sample_display, sample_trip,
};

pub(super) fn render_catalogue_inspector(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    train: Option<&TrainModel>,
    wide: bool,
    embedded: bool,
) {
    let inner = if embedded {
        let divider = Block::default()
            .borders(if wide { Borders::LEFT } else { Borders::TOP })
            .border_style(theme::border())
            .style(theme::panel());
        let inner = horizontal_inset(divider.inner(area), 1);
        frame.render_widget(divider, area);
        inner
    } else {
        area
    };

    let Some(train) = train else {
        frame.render_widget(
            Paragraph::new("No Train model selected").style(theme::panel()),
            inner,
        );
        return;
    };

    let dense = !wide || inner.height < 24;
    let ownership = catalogue_ownership(state, train);
    let (status, status_style) = purchase_status(state, train);

    let mut lines = vec![
        section_heading("SELECTED MODEL"),
        Line::styled(train.name().to_owned(), theme::focused_title()),
        Line::from(vec![
            Span::styled(train.evn_type_label(), theme::primary_value()),
            Span::styled(
                format!(
                    " · {} seats · {} · {}/km",
                    train.passenger_capacity().passengers(),
                    format_speed_kmh(train),
                    format_money_per_kilometre(
                        train.fuel_cost_per_kilometre().cents_per_kilometre()
                    )
                ),
                theme::secondary(),
            ),
        ]),
    ];

    if let Some(hint) = purchase_status_hint(state, train) {
        lines.push(hint);
    }

    inspector_section(&mut lines, "ACQUISITION", dense);
    if !dense {
        lines.push(labelled_line_styled("Order status", status, status_style));
    }
    lines.push(labelled_line(
        "Purchase price",
        &format_money(train.purchase_price()),
    ));
    lines.push(purchase_balance_line(state, train));

    // Registration deliberately moves ahead of secondary comparison data in
    // dense layouts so the EVN detail survives narrow/short terminals.
    if dense {
        append_registration(&mut lines, state, train, true);
        append_operating_profile(&mut lines, state, train, true);
    } else {
        append_operating_profile(&mut lines, state, train, false);
        append_registration(&mut lines, state, train, false);
    }
    append_fleet_presence(&mut lines, ownership, dense);

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::panel())
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn append_operating_profile(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    train: &TrainModel,
    dense: bool,
) {
    inspector_section(lines, "OPERATING PROFILE", dense);
    lines.push(labelled_line(
        "Fuel cost",
        &format!(
            "{}/km",
            format_money_per_kilometre(train.fuel_cost_per_kilometre().cents_per_kilometre())
        ),
    ));
    if let Some(sample) = sample_trip(state, train) {
        lines.push(labelled_line("Benchmark", &sample.route));
        lines.push(labelled_line("Distance", &sample.distance));
        if !dense {
            lines.push(labelled_line(
                "Trip cost",
                &format_money(sample.departure_cost),
            ));
            lines.push(labelled_line(
                "Cash after trip",
                &reserve_after_sample_display(state, train, &sample),
            ));
        }
    } else if !dense {
        lines.push(labelled_line("Benchmark", "Unavailable"));
    }
}

fn append_registration(
    lines: &mut Vec<Line<'static>>,
    state: &GameState,
    train: &TrainModel,
    dense: bool,
) {
    inspector_section(lines, "REGISTRATION", dense);
    let keeper_mark = format!(
        "{}-{}",
        state.region.railway_registration.mark,
        state.player_company.vehicle_keeper_mark.as_str(),
    );

    if dense {
        lines.push(labelled_line(
            "EVN basis",
            &format!(
                "{:02} {:02} {:04} · {}",
                train.evn_type_code(),
                state.region.railway_registration.numeric_code,
                train.evn_series_code(),
                state.region.railway_registration.mark
            ),
        ));
    } else {
        lines.push(labelled_line(
            "EVN type",
            &format!("{:02}", train.evn_type_code()),
        ));
        lines.push(labelled_line(
            "EVN series",
            &format!("{:04}", train.evn_series_code()),
        ));
        lines.push(labelled_line(
            "Registration",
            &format!(
                "{:02} · {}",
                state.region.railway_registration.numeric_code,
                state.region.railway_registration.mark
            ),
        ));
    }
    lines.push(labelled_line("Keeper mark", &keeper_mark));
    lines.push(labelled_line("Official EVN", "Assigned on purchase"));
}

fn append_fleet_presence(
    lines: &mut Vec<Line<'static>>,
    ownership: CatalogueOwnership,
    dense: bool,
) {
    inspector_section(lines, "FLEET PRESENCE", dense);
    lines.push(labelled_line("Owned", &ownership.owned.to_string()));
    if dense {
        lines.push(labelled_line(
            "Availability",
            &format!(
                "{} ready · {} travelling",
                ownership.ready, ownership.travelling
            ),
        ));
    } else {
        lines.push(labelled_line("Ready", &ownership.ready.to_string()));
        lines.push(labelled_line(
            "Travelling",
            &ownership.travelling.to_string(),
        ));
    }
}

fn inspector_section(lines: &mut Vec<Line<'static>>, title: &str, dense: bool) {
    if !dense {
        lines.push(Line::from(""));
    }
    lines.push(section_heading(title));
}

fn purchase_status_hint(state: &GameState, train: &TrainModel) -> Option<Line<'static>> {
    if state.player_company.funds < train.purchase_price() {
        return Some(Line::styled(
            "Company Funds are below this purchase price.",
            theme::error(),
        ));
    }
    if delivery_station_ids(state).is_empty() {
        return Some(Line::styled(
            "Connect a Rail Station before buying.",
            theme::warning(),
        ));
    }
    if low_reserve(state, train) {
        return Some(Line::styled(
            "Purchase leaves less cash than the benchmark trip requires.",
            theme::warning(),
        ));
    }
    None
}

fn purchase_balance_line(state: &GameState, train: &TrainModel) -> Line<'static> {
    if state.player_company.funds < train.purchase_price() {
        let shortfall = train
            .purchase_price()
            .checked_sub(state.player_company.funds)
            .map(format_money)
            .unwrap_or_else(|_| "Unavailable".into());
        return labelled_line_styled("Shortfall", &shortfall, theme::error());
    }

    labelled_line(
        "Cash after",
        &funds_after_purchase_display(state, train),
    )
}

fn purchase_status(
    state: &GameState,
    train: &TrainModel,
) -> (&'static str, ratatui::style::Style) {
    if state.player_company.funds < train.purchase_price() {
        ("INSUFFICIENT FUNDS", theme::error())
    } else if delivery_station_ids(state).is_empty() {
        ("DELIVERY UNAVAILABLE", theme::warning())
    } else if low_reserve(state, train) {
        ("LOW RESERVE", theme::warning())
    } else {
        ("READY TO ORDER", theme::success())
    }
}
