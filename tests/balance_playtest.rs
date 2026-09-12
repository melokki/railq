//! Deterministic balance playtests using saved economic rules and the embedded Train catalogue.
//!
//! These scenarios use explicit timestamps so they exercise the same elapsed
//! time rules as the game without waiting for real-world Journeys.

use railq::{
    model::{GameState, Money, RailStationId, UtcSeconds},
    sim::{
        economy::{JourneyQuote, quote_journey},
        finance::{FinancialStatus, RecoveryOption, evaluate_financial_recovery},
        fleet::{purchase_train, sell_train},
        journeys::dispatch_journey,
        services::find_or_create_service,
        time::advance_time,
        world::create_new_game,
    },
};

const A: RailStationId = RailStationId::new(1);
const B: RailStationId = RailStationId::new(2);

fn starter_game(seed: u64) -> (GameState, railq::model::TrainId, railq::model::ServiceId) {
    let mut state = create_new_game(seed, "Balance Passenger", UtcSeconds::from_unix_seconds(0));
    let train_id = purchase_train(&mut state, 0, A).expect("starter Local purchase succeeds");
    let service_id =
        find_or_create_service(&mut state, A, B).expect("short Passenger Service is created");
    (state, train_id, service_id)
}

fn dispatch_and_arrive(
    state: &mut GameState,
    train_id: railq::model::TrainId,
    service_id: railq::model::ServiceId,
) -> JourneyQuote {
    let quote = quote_journey(state, train_id, service_id).expect("Journey is quotable");
    let journey_id = dispatch_journey(state, train_id, service_id, state.last_processed_at)
        .expect("Manual Dispatch succeeds");
    let arrives_at = state
        .active_journeys
        .iter()
        .find(|journey| journey.id == journey_id)
        .expect("dispatched Journey is active")
        .arrives_at;
    advance_time(state, arrives_at).expect("injected arrival settles");
    quote
}

fn drain_company_funds(
    state: &mut GameState,
    train_id: railq::model::TrainId,
    service_id: railq::model::ServiceId,
    target: Money,
) {
    for _ in 0..2_000 {
        if state.player_company.funds < target {
            return;
        }
        dispatch_and_arrive(state, train_id, service_id);
    }
    panic!("low-demand Manual Dispatches did not reach the target Company Funds");
}

fn grow_to_second_train_funds(
    state: &mut GameState,
    train_id: railq::model::TrainId,
    service_id: railq::model::ServiceId,
) {
    assert!(dispatch_and_arrive(state, train_id, service_id).journey_profitability > Money::ZERO);
    for day in 1..=8 {
        advance_time(state, UtcSeconds::from_unix_seconds(day * 86_400))
            .expect("injected demand accumulation succeeds");
        let quote = dispatch_and_arrive(state, train_id, service_id);
        assert_eq!(quote.boarded_passengers, 70);
        assert!(quote.journey_profitability > Money::ZERO);
    }
}

#[test]
fn several_seeded_starter_markets_are_profitable() {
    for seed in [1, 12, 42, 99] {
        let (state, train_id, service_id) = starter_game(seed);
        let quote = quote_journey(&state, train_id, service_id).expect("starter Journey quotes");

        assert!(quote.boarded_passengers >= 15, "seed {seed}");
        assert!(quote.journey_profitability > Money::ZERO, "seed {seed}");
        assert!(quote.cash_after_cost >= Money::ZERO, "seed {seed}");
    }
}

#[test]
fn finite_directional_demand_makes_immediate_over_service_lose_money() {
    let (mut state, train_id, service_id) = starter_game(12);

    assert!(
        dispatch_and_arrive(&mut state, train_id, service_id).journey_profitability > Money::ZERO
    );
    assert!(
        dispatch_and_arrive(&mut state, train_id, service_id).journey_profitability > Money::ZERO
    );

    let funds_before = state.player_company.funds;
    let over_service = dispatch_and_arrive(&mut state, train_id, service_id);

    assert!(over_service.boarded_passengers < 3);
    assert!(over_service.journey_profitability < Money::ZERO);
    assert!(state.player_company.funds < funds_before);
}

#[test]
fn resale_of_one_train_recovers_an_insolvent_company() {
    let (mut state, first_train_id, service_id) = starter_game(12);

    grow_to_second_train_funds(&mut state, first_train_id, service_id);
    let second_train_id = purchase_train(&mut state, 0, A).expect("growth reaches a second Train");

    drain_company_funds(
        &mut state,
        first_train_id,
        service_id,
        Money::from_cents(570),
    );

    let evaluation = evaluate_financial_recovery(&state).expect("recovery is evaluated");

    assert_eq!(evaluation.status, FinancialStatus::Insolvent);
    assert!(evaluation.recovery_options.iter().any(|option| {
        matches!(option, RecoveryOption::SellOthersAndRetain {
            retained_train_id,
            sold_train_ids,
            resale_proceeds,
            ..
        } if *retained_train_id == first_train_id
            && sold_train_ids == &vec![second_train_id]
            && *resale_proceeds == Money::from_cents(210_000))
    }));

    assert_eq!(
        sell_train(&mut state, second_train_id).expect("other ready Train is sold"),
        Money::from_cents(210_000)
    );
    assert!(
        quote_journey(&state, first_train_id, service_id)
            .expect("retained Train is quotable")
            .operating_cost
            <= state.player_company.funds
    );
    dispatch_and_arrive(&mut state, first_train_id, service_id);
}

#[test]
fn company_growth_can_buy_a_second_train_with_injected_time() {
    let (mut state, train_id, service_id) = starter_game(12);

    grow_to_second_train_funds(&mut state, train_id, service_id);

    assert!(purchase_train(&mut state, 0, A).is_ok());
}

#[test]
fn sustained_low_demand_and_failed_resale_recovery_reach_bankruptcy() {
    let (mut state, train_id, service_id) = starter_game(12);

    dispatch_and_arrive(&mut state, train_id, service_id);
    dispatch_and_arrive(&mut state, train_id, service_id);
    drain_company_funds(&mut state, train_id, service_id, Money::from_cents(90_000));
    let proceeds = sell_train(&mut state, train_id).expect("ready Train can be sold");

    assert_eq!(proceeds, Money::from_cents(210_000));
    assert_eq!(
        evaluate_financial_recovery(&state)
            .expect("Bankruptcy is evaluated")
            .status,
        FinancialStatus::Bankruptcy
    );
}
