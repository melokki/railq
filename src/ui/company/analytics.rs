use crate::model::{GameState, JourneyReceipt};

/// Number of the most recently completed Journeys used for dashboard context.
pub(super) const RECENT_JOURNEY_WINDOW: usize = 12;

/// Financial and passenger summary for the latest completed Journeys.
///
/// This is presentation-only analytics. It deliberately works from retained
/// receipts instead of pretending the Company has a timestamped accounting
/// ledger for a UTC day.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecentJourneyPerformance {
    pub(super) journey_count: usize,
    pub(super) revenue_cents: i128,
    pub(super) operating_costs_cents: i128,
    pub(super) result_cents: i128,
    pub(super) profitable_journeys: usize,
    pub(super) loss_making_journeys: usize,
    /// Total boardings only when every receipt in the window carries passenger
    /// context. Legacy receipts keep this unknown rather than being counted as 0.
    pub(super) passengers_carried: Option<u64>,
}

/// Live exposure for Journeys that have departed but have not yet completed.
///
/// Revenue in transit is booked passenger revenue that has not yet been
/// credited to Company funds. It is intentionally separate from cash and from
/// settled Journey performance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ActiveJourneyExposure {
    pub(super) journey_count: usize,
    pub(super) onboard_passengers: u64,
    pub(super) revenue_in_transit_cents: i128,
}

impl ActiveJourneyExposure {
    pub(super) fn from_state(state: &GameState) -> Self {
        let mut exposure = Self {
            journey_count: state.active_journeys.len(),
            onboard_passengers: 0,
            revenue_in_transit_cents: 0,
        };

        for journey in &state.active_journeys {
            let booked = i128::from(journey.operating_revenue.cents());
            let credited = i128::from(journey.credited_revenue.cents());
            exposure.onboard_passengers = exposure
                .onboard_passengers
                .saturating_add(u64::from(journey.onboard_passengers()));
            exposure.revenue_in_transit_cents += booked - credited;
        }

        exposure
    }
}

impl RecentJourneyPerformance {
    pub(super) fn from_state(state: &GameState) -> Self {
        Self::from_receipts(&state.financials.recent_journey_receipts)
    }

    fn from_receipts(receipts: &[JourneyReceipt]) -> Self {
        let mut performance = Self {
            journey_count: 0,
            revenue_cents: 0,
            operating_costs_cents: 0,
            result_cents: 0,
            profitable_journeys: 0,
            loss_making_journeys: 0,
            passengers_carried: Some(0),
        };

        for receipt in receipts.iter().rev().take(RECENT_JOURNEY_WINDOW) {
            let revenue_cents = i128::from(receipt.revenue.cents());
            let operating_costs_cents = i128::from(receipt.infrastructure_access_fee.cents())
                + i128::from(receipt.fuel_cost.cents());
            let result_cents = revenue_cents - operating_costs_cents;

            performance.journey_count += 1;
            performance.revenue_cents += revenue_cents;
            performance.operating_costs_cents += operating_costs_cents;
            performance.result_cents += result_cents;
            if result_cents > 0 {
                performance.profitable_journeys += 1;
            } else if result_cents < 0 {
                performance.loss_making_journeys += 1;
            }

            performance.passengers_carried = match (
                performance.passengers_carried,
                receipt.passengers_carried,
            ) {
                (Some(total), Some(passengers)) => Some(total + u64::from(passengers)),
                _ => None,
            };
        }

        performance
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{JourneyId, JourneyReceipt, Money};

    use super::{ActiveJourneyExposure, RECENT_JOURNEY_WINDOW, RecentJourneyPerformance};

    fn receipt(
        id: u64,
        revenue_cents: i64,
        access_cents: i64,
        fuel_cents: i64,
        passengers: Option<u32>,
    ) -> JourneyReceipt {
        JourneyReceipt {
            journey_id: JourneyId::new(id),
            revenue: Money::from_cents(revenue_cents),
            infrastructure_access_fee: Money::from_cents(access_cents),
            fuel_cost: Money::from_cents(fuel_cents),
            train_id: None,
            train_model_name: None,
            origin_station_id: None,
            destination_station_id: None,
            passengers_carried: passengers,
            passenger_capacity: None,
            completed_at: None,
        }
    }

    #[test]
    fn recent_performance_uses_only_the_latest_window() {
        let mut receipts = vec![
            receipt(1, 100_000, 0, 0, Some(999)),
            receipt(2, 100_000, 0, 0, Some(999)),
        ];
        receipts.extend((3..=14).map(|id| receipt(id, 10_000, 1_000, 500, Some(20))));

        let performance = RecentJourneyPerformance::from_receipts(&receipts);

        assert_eq!(performance.journey_count, RECENT_JOURNEY_WINDOW);
        assert_eq!(performance.revenue_cents, 120_000);
        assert_eq!(performance.operating_costs_cents, 18_000);
        assert_eq!(performance.result_cents, 102_000);
        assert_eq!(performance.profitable_journeys, 12);
        assert_eq!(performance.loss_making_journeys, 0);
        assert_eq!(performance.passengers_carried, Some(240));
    }

    #[test]
    fn recent_performance_classifies_profit_and_loss_without_hiding_break_even() {
        let receipts = vec![
            receipt(1, 10_000, 1_000, 500, Some(40)),
            receipt(2, 1_000, 1_000, 500, Some(10)),
            receipt(3, 1_500, 1_000, 500, Some(0)),
        ];

        let performance = RecentJourneyPerformance::from_receipts(&receipts);

        assert_eq!(performance.profitable_journeys, 1);
        assert_eq!(performance.loss_making_journeys, 1);
        assert_eq!(
            performance.journey_count
                - performance.profitable_journeys
                - performance.loss_making_journeys,
            1
        );
        assert_eq!(performance.result_cents, 8_000);
    }

    #[test]
    fn legacy_passenger_context_stays_unknown_instead_of_becoming_zero() {
        let receipts = vec![
            receipt(1, 10_000, 1_000, 500, Some(40)),
            receipt(2, 10_000, 1_000, 500, None),
        ];

        let performance = RecentJourneyPerformance::from_receipts(&receipts);

        assert_eq!(performance.passengers_carried, None);
    }

    #[test]
    fn active_exposure_tracks_booked_uncredited_revenue() {
        use crate::{
            model::{
                MarketMaturity, MoneyPerKilometre, PassengerArrivalRate, RailStationId, UtcSeconds,
            },
            sim::{
                fleet::purchase_train, journeys::dispatch_journey,
                services::find_or_create_service, world::create_new_game,
            },
        };

        let started_at = UtcSeconds::from_unix_seconds(1_700_000_000);
        let origin = RailStationId::new(1);
        let destination = RailStationId::new(2);
        let mut state = create_new_game(42, "Exposure Test", started_at);
        state.rules.balance = crate::balance::BalanceConfig::new(
            MoneyPerKilometre::new(10).expect("fare rate is valid"),
            MoneyPerKilometre::new(10).expect("access rate is valid"),
            Money::from_cents(400_000),
        );
        state.player_company.funds = Money::from_cents(400_000);
        for pool in &mut state.origin_destination_demand {
            pool.waiting_passengers = 0;
            pool.passenger_arrival_rate_per_hour =
                PassengerArrivalRate::new(1).expect("demand rate is valid");
            pool.market_maturity = MarketMaturity::full();
            pool.fractional_passenger_seconds = 0;
        }
        state
            .origin_destination_demand
            .iter_mut()
            .find(|pool| {
                pool.origin_station_id == origin && pool.destination_station_id == destination
            })
            .expect("fixture route exists")
            .waiting_passengers = 10;

        let train_id = purchase_train(&mut state, 0, origin).expect("purchase succeeds");
        let service_id =
            find_or_create_service(&mut state, origin, destination).expect("service exists");
        dispatch_journey(&mut state, train_id, service_id, started_at)
            .expect("Journey departs");

        let exposure = ActiveJourneyExposure::from_state(&state);
        let journey = &state.active_journeys[0];
        assert_eq!(exposure.journey_count, 1);
        assert_eq!(exposure.onboard_passengers, 10);
        assert_eq!(
            exposure.revenue_in_transit_cents,
            i128::from(journey.operating_revenue.cents())
                - i128::from(journey.credited_revenue.cents())
        );
    }
}
