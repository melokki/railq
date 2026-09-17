//! Tunable economic rules saved with each game.
//!
//! Static Train definitions are intentionally not stored here. They live in
//! the central RON catalogue (`assets/trains.ron`) and are shared by every
//! game in this RailQ build.

use serde::{Deserialize, Serialize};

use crate::model::{Money, MoneyPerKilometre};

/// The tunable economy for one game.
///
/// Rates are in cents per kilometre. Fare is charged once per passenger and
/// access is charged once per Train Journey. Train-specific fuel economics are
/// part of each immutable [`crate::catalog::TrainModel`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BalanceConfig {
    fare_per_passenger_kilometre: MoneyPerKilometre,
    access_fee_per_train_kilometre: MoneyPerKilometre,
    starting_company_funds: Money,
}

impl BalanceConfig {
    pub fn new(
        fare_per_passenger_kilometre: MoneyPerKilometre,
        access_fee_per_train_kilometre: MoneyPerKilometre,
        starting_company_funds: Money,
    ) -> Self {
        Self {
            fare_per_passenger_kilometre,
            access_fee_per_train_kilometre,
            starting_company_funds,
        }
    }

    /// Creates the initial playtest balance. Every value in this method is a
    /// deliberately tunable provisional default, rather than a design rule.
    pub fn provisional() -> Self {
        let fare_per_passenger_kilometre =
            MoneyPerKilometre::new(12).expect("tunable fare rate must remain positive");
        let access_fee_per_train_kilometre =
            MoneyPerKilometre::new(35).expect("tunable access rate must remain positive");
        let starting_company_funds = Money::from_cents(500_000);

        Self::new(
            fare_per_passenger_kilometre,
            access_fee_per_train_kilometre,
            starting_company_funds,
        )
    }

    pub const fn fare_per_passenger_kilometre(&self) -> MoneyPerKilometre {
        self.fare_per_passenger_kilometre
    }

    pub const fn access_fee_per_train_kilometre(&self) -> MoneyPerKilometre {
        self.access_fee_per_train_kilometre
    }

    pub const fn starting_company_funds(&self) -> Money {
        self.starting_company_funds
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::train_catalogue;

    use super::*;

    #[test]
    fn provisional_balance_uses_rebalanced_operating_rates() {
        let balance = BalanceConfig::provisional();

        assert_eq!(
            balance
                .fare_per_passenger_kilometre()
                .cents_per_kilometre(),
            12
        );
        assert_eq!(
            balance
                .access_fee_per_train_kilometre()
                .cents_per_kilometre(),
            35
        );
        assert_eq!(balance.starting_company_funds(), Money::from_cents(500_000));
    }

    #[test]
    fn keeps_all_tunable_economic_values_in_the_saved_configuration() {
        let rate = MoneyPerKilometre::new(25).unwrap();
        let balance = BalanceConfig::new(rate, rate, Money::from_cents(100));

        assert_eq!(balance.fare_per_passenger_kilometre(), rate);
        assert_eq!(balance.access_fee_per_train_kilometre(), rate);
        assert_eq!(balance.starting_company_funds(), Money::from_cents(100));
    }

    #[test]
    fn embedded_catalogue_has_the_intended_initial_tradeoffs() {
        let [local, express] = train_catalogue().models() else {
            panic!("the v0.1 catalogue must contain exactly two Train models");
        };

        assert!(local.purchase_price() < express.purchase_price());
        assert!(local.passenger_capacity() < express.passenger_capacity());
        assert!(local.speed() < express.speed());
        assert!(local.fuel_cost_per_kilometre() > express.fuel_cost_per_kilometre());
    }

    #[test]
    fn provisional_starting_funds_cover_a_local_purchase_and_departure() {
        let balance = BalanceConfig::provisional();
        let local = &train_catalogue().models()[0];
        let starter_connection = crate::model::DistanceMetres::new(10_000).unwrap();
        let departure_cost = balance
            .access_fee_per_train_kilometre()
            .checked_charge(starter_connection)
            .unwrap()
            .checked_add(
                local
                    .fuel_cost_per_kilometre()
                    .checked_charge(starter_connection)
                    .unwrap(),
            )
            .unwrap();

        let funds_after_purchase = balance
            .starting_company_funds()
            .checked_sub(local.purchase_price())
            .unwrap();
        assert!(funds_after_purchase >= departure_cost);
    }
}
