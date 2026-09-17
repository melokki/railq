//! End-to-end regression scenarios for the Player Company's operating loop.
//!
//! These fixtures deliberately use literal, hand-calculated amounts. The
//! expected economics must not be derived from the production quote logic.

use railq::{
    balance::BalanceConfig,
    catalog::train_catalogue,
    model::{
        GameState, MarketMaturity, Money, MoneyPerKilometre, PassengerArrivalRate, RailStationId,
        TrainStatus, UtcSeconds,
    },
    sim::{
        economy::quote_journey,
        fleet::{purchase_train, sell_train},
        journeys::dispatch_journey,
        services::find_or_create_service,
        time::advance_time,
    },
};

mod support;

use support::new_game;

const ORIGIN: RailStationId = RailStationId::new(1);
const DESTINATION: RailStationId = RailStationId::new(2);
const DEPARTURE: UtcSeconds = UtcSeconds::from_unix_seconds(0);
const FIRST_ARRIVAL: UtcSeconds = UtcSeconds::from_unix_seconds(304);
const RETURN_ARRIVAL: UtcSeconds = UtcSeconds::from_unix_seconds(608);

// The fixture's selected Rail Line is 10 km. At 10 cents per passenger-km,
// each Passenger carried pays 100 cents. The embedded Helvetra R70 costs 120 cents
// per kilometre in fuel and runs at 33 m/s, so one Journey costs 100 cents
// access + 1,200 cents fuel and lasts 304 seconds.
const PURCHASE_PRICE: Money = Money::from_cents(300_000);
const RESALE_PROCEEDS: Money = Money::from_cents(210_000);
const FARE_PER_PASSENGER: Money = Money::from_cents(100);
const ACCESS_FEE: Money = Money::from_cents(100);
const FUEL_COST: Money = Money::from_cents(1_200);
const OPERATING_COST: Money = Money::from_cents(1_300);

// The provisional economy uses the same 10 km starter connection, but keeps
// the production balance values instead of the custom 10/10 regression rates
// above. One R70 Journey therefore costs 350 cents access + 1,200 cents fuel,
// while each Passenger pays 120 cents.
const PROVISIONAL_FARE_PER_PASSENGER: Money = Money::from_cents(120);
const PROVISIONAL_ACCESS_FEE: Money = Money::from_cents(350);
const PROVISIONAL_FUEL_COST: Money = Money::from_cents(1_200);
const PROVISIONAL_OPERATING_COST: Money = Money::from_cents(1_550);

fn operating_game(company_funds: Money) -> GameState {
    let fare_rate = MoneyPerKilometre::new(10).expect("fixture fare rate is positive");
    let access_rate = MoneyPerKilometre::new(10).expect("fixture access rate is positive");
    let mut state = new_game("Regression Passenger", DEPARTURE);

    state.rules.balance = BalanceConfig::new(fare_rate, access_rate, company_funds);
    state.player_company.funds = company_funds;
    for pool in &mut state.origin_destination_demand {
        pool.waiting_passengers = 0;
        pool.passenger_arrival_rate_per_hour =
            PassengerArrivalRate::new(1).expect("fixture demand rate is positive");
        pool.market_maturity = MarketMaturity::full();
        pool.fractional_passenger_seconds = 0;
    }

    assert_eq!(
        train_catalogue().models()[0].purchase_price(),
        PURCHASE_PRICE,
        "the operating-loop fixture intentionally exercises the embedded Helvetra R70"
    );
    state
}

fn set_waiting_passengers(
    state: &mut GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
    passengers: u32,
) {
    state
        .origin_destination_demand
        .iter_mut()
        .find(|pool| {
            pool.origin_station_id == origin_station_id
                && pool.destination_station_id == destination_station_id
        })
        .expect("the connected directional pool exists")
        .waiting_passengers = passengers;
}

fn waiting_passengers(
    state: &GameState,
    origin_station_id: RailStationId,
    destination_station_id: RailStationId,
) -> u32 {
    state
        .origin_destination_demand
        .iter()
        .find(|pool| {
            pool.origin_station_id == origin_station_id
                && pool.destination_station_id == destination_station_id
        })
        .expect("the connected directional pool exists")
        .waiting_passengers
}

fn provisional_r70_quote(passengers: u32) -> railq::sim::economy::JourneyQuote {
    let mut state = new_game("Provisional Economy Passenger", DEPARTURE);
    for pool in &mut state.origin_destination_demand {
        pool.waiting_passengers = 0;
    }
    set_waiting_passengers(&mut state, ORIGIN, DESTINATION, passengers);

    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("starter R70 purchase succeeds");
    let service_id = find_or_create_service(&mut state, ORIGIN, DESTINATION)
        .expect("starter Service is created");

    quote_journey(&state, train_id, service_id).expect("starter Journey is quotable")
}

#[test]
fn provisional_r70_load_cases_match_hand_calculated_economics() {
    // Passenger count, revenue cents, Journey profit cents.
    let cases = [
        (0, 0, -1_550),
        (7, 840, -710),
        (35, 4_200, 2_650),
        (70, 8_400, 6_850),
    ];

    for (passengers, revenue_cents, profit_cents) in cases {
        let quote = provisional_r70_quote(passengers);

        assert_eq!(quote.boarded_passengers, passengers);
        assert_eq!(quote.fare, PROVISIONAL_FARE_PER_PASSENGER);
        assert_eq!(quote.operating_revenue, Money::from_cents(revenue_cents));
        assert_eq!(quote.infrastructure_access_fee, PROVISIONAL_ACCESS_FEE);
        assert_eq!(quote.fuel_cost, PROVISIONAL_FUEL_COST);
        assert_eq!(quote.operating_cost, PROVISIONAL_OPERATING_COST);
        assert_eq!(quote.journey_profitability, Money::from_cents(profit_cents));
    }
}

#[test]
fn provisional_r70_break_even_requires_thirteen_passengers() {
    let twelve_passengers = provisional_r70_quote(12);
    let thirteen_passengers = provisional_r70_quote(13);

    assert_eq!(
        twelve_passengers.operating_revenue,
        Money::from_cents(1_440)
    );
    assert_eq!(
        twelve_passengers.journey_profitability,
        Money::from_cents(-110)
    );
    assert_eq!(
        thirteen_passengers.operating_revenue,
        Money::from_cents(1_560)
    );
    assert_eq!(
        thirteen_passengers.journey_profitability,
        Money::from_cents(10)
    );

    // 13 / 70 seats = 18.57% occupancy, matching the intended ~18.5%
    // operating break-even before future maintenance and wear costs.
    assert_eq!(
        train_catalogue().models()[0]
            .passenger_capacity()
            .passengers(),
        70
    );
}

#[test]
fn buy_service_dispatch_arrive_return_and_sell_has_hand_calculated_profit() {
    let mut state = operating_game(Money::from_cents(400_000));
    set_waiting_passengers(&mut state, ORIGIN, DESTINATION, 4);
    set_waiting_passengers(&mut state, DESTINATION, ORIGIN, 3);

    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("purchase succeeds");
    assert_eq!(state.player_company.funds, Money::from_cents(100_000));
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("Service is created");
    assert_eq!(state.player_company.passenger_services.len(), 1);

    let outbound = quote_journey(&state, train_id, service_id).expect("outbound is quotable");
    assert_eq!(outbound.boarded_passengers, 4);
    assert_eq!(outbound.fare, FARE_PER_PASSENGER);
    assert_eq!(outbound.operating_revenue, Money::from_cents(400));
    assert_eq!(outbound.infrastructure_access_fee, ACCESS_FEE);
    assert_eq!(outbound.fuel_cost, FUEL_COST);
    assert_eq!(outbound.operating_cost, OPERATING_COST);
    assert_eq!(outbound.journey_profitability, Money::from_cents(-900));

    let outbound_journey =
        dispatch_journey(&mut state, train_id, service_id, DEPARTURE).expect("outbound departs");
    assert_eq!(state.player_company.funds, Money::from_cents(98_700));
    assert_eq!(
        state.player_company.fleet.trains[0].status,
        TrainStatus::Travelling {
            journey_id: outbound_journey
        }
    );
    advance_time(&mut state, FIRST_ARRIVAL).expect("outbound settles");
    assert_eq!(state.player_company.funds, Money::from_cents(99_100));
    assert_eq!(
        state.player_company.fleet.trains[0].status,
        TrainStatus::Ready { at: DESTINATION }
    );

    let return_trip = quote_journey(&state, train_id, service_id).expect("return is quotable");
    assert_eq!(return_trip.boarded_passengers, 3);
    assert_eq!(return_trip.operating_revenue, Money::from_cents(300));
    assert_eq!(return_trip.operating_cost, OPERATING_COST);
    assert_eq!(return_trip.journey_profitability, Money::from_cents(-1_000));
    dispatch_journey(&mut state, train_id, service_id, FIRST_ARRIVAL).expect("return departs");
    assert_eq!(state.player_company.funds, Money::from_cents(97_800));
    advance_time(&mut state, RETURN_ARRIVAL).expect("return settles");

    assert_eq!(state.player_company.funds, Money::from_cents(98_100));
    assert_eq!(state.financials.operating_revenue, Money::from_cents(700));
    assert_eq!(
        state.financials.infrastructure_access_fees,
        Money::from_cents(200)
    );
    assert_eq!(state.financials.fuel_costs, Money::from_cents(2_400));
    assert_eq!(sell_train(&mut state, train_id), Ok(RESALE_PROCEEDS));
    assert_eq!(state.player_company.funds, Money::from_cents(308_100));
    assert!(state.player_company.fleet.trains.is_empty());
}

#[test]
fn two_trains_take_only_their_shared_finite_directional_demand() {
    let mut state = operating_game(Money::from_cents(700_000));
    set_waiting_passengers(&mut state, ORIGIN, DESTINATION, 100);

    let first_train = purchase_train(&mut state, 0, ORIGIN).expect("first Train is bought");
    let second_train = purchase_train(&mut state, 0, ORIGIN).expect("second Train is bought");
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("Service is created");

    dispatch_journey(&mut state, first_train, service_id, DEPARTURE).expect("first departs");
    dispatch_journey(&mut state, second_train, service_id, DEPARTURE).expect("second departs");

    assert_eq!(waiting_passengers(&state, ORIGIN, DESTINATION), 0);
    assert_eq!(state.active_journeys[0].passengers_carried, 70);
    assert_eq!(state.active_journeys[1].passengers_carried, 30);
    assert_eq!(
        state.active_journeys[0].operating_revenue,
        Money::from_cents(7_000)
    );
    assert_eq!(
        state.active_journeys[1].operating_revenue,
        Money::from_cents(3_000)
    );

    advance_time(&mut state, FIRST_ARRIVAL).expect("both Journeys settle");
    assert_eq!(state.player_company.funds, Money::from_cents(107_400));
    assert_eq!(
        state.financials.operating_revenue,
        Money::from_cents(10_000)
    );
}

#[test]
fn empty_repositioning_journey_has_zero_revenue_and_costs_money() {
    let mut state = operating_game(Money::from_cents(400_000));
    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("Train is bought");
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("Service is created");

    dispatch_journey(&mut state, train_id, service_id, DEPARTURE).expect("empty Journey departs");
    let journey = &state.active_journeys[0];
    assert_eq!(journey.passengers_carried, 0);
    assert_eq!(journey.operating_revenue, Money::ZERO);
    assert_eq!(journey.infrastructure_access_fee, ACCESS_FEE);
    assert_eq!(journey.fuel_cost, FUEL_COST);
    assert_eq!(state.player_company.funds, Money::from_cents(98_700));

    advance_time(&mut state, FIRST_ARRIVAL).expect("empty Journey settles");
    assert_eq!(state.player_company.funds, Money::from_cents(98_700));
    assert_eq!(state.financials.operating_revenue, Money::ZERO);
}

#[test]
fn one_passenger_journey_is_loss_making_by_twelve_dollars() {
    let mut state = operating_game(Money::from_cents(400_000));
    set_waiting_passengers(&mut state, ORIGIN, DESTINATION, 1);
    let train_id = purchase_train(&mut state, 0, ORIGIN).expect("Train is bought");
    let service_id =
        find_or_create_service(&mut state, ORIGIN, DESTINATION).expect("Service is created");

    let quote = quote_journey(&state, train_id, service_id).expect("Journey is quotable");
    assert_eq!(quote.boarded_passengers, 1);
    assert_eq!(quote.operating_revenue, FARE_PER_PASSENGER);
    assert_eq!(quote.operating_cost, OPERATING_COST);
    assert_eq!(quote.journey_profitability, Money::from_cents(-1_200));
    dispatch_journey(&mut state, train_id, service_id, DEPARTURE).expect("Journey departs");
    advance_time(&mut state, FIRST_ARRIVAL).expect("Journey settles");

    assert_eq!(state.player_company.funds, Money::from_cents(98_800));
    assert_eq!(state.financials.operating_revenue, FARE_PER_PASSENGER);
    assert_eq!(state.financials.infrastructure_access_fees, ACCESS_FEE);
    assert_eq!(state.financials.fuel_costs, FUEL_COST);
}
