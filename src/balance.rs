//! Tunable economic rules saved with each game.

use serde::{Deserialize, Serialize};

use crate::model::{Money, MoneyPerKilometre, PassengerCapacity, SpeedMetresPerSecond};

/// One diesel Train available for purchase in the v0.1 catalogue.
///
/// These records are part of [`BalanceConfig`], so a saved game retains the
/// catalogue prices and Train economics it started with when playtest values
/// are tuned later.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DieselTrainCatalogueRecord {
    name: String,
    purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

impl DieselTrainCatalogueRecord {
    pub fn new(
        name: impl Into<String>,
        purchase_price: Money,
        passenger_capacity: PassengerCapacity,
        speed: SpeedMetresPerSecond,
        fuel_cost_per_kilometre: MoneyPerKilometre,
    ) -> Self {
        Self {
            name: name.into(),
            purchase_price,
            passenger_capacity,
            speed,
            fuel_cost_per_kilometre,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn purchase_price(&self) -> Money {
        self.purchase_price
    }

    pub const fn passenger_capacity(&self) -> PassengerCapacity {
        self.passenger_capacity
    }

    pub const fn speed(&self) -> SpeedMetresPerSecond {
        self.speed
    }

    pub const fn fuel_cost_per_kilometre(&self) -> MoneyPerKilometre {
        self.fuel_cost_per_kilometre
    }
}

/// The tunable economy and diesel stock for one game.
///
/// Rates are in cents per kilometre. Fare is charged once per passenger and
/// access is charged once per Train Journey. Fuel cost belongs to each diesel
/// catalogue record because different Trains have different fuel economics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BalanceConfig {
    fare_per_passenger_kilometre: MoneyPerKilometre,
    access_fee_per_train_kilometre: MoneyPerKilometre,
    starting_company_funds: Money,
    diesel_catalogue: Vec<DieselTrainCatalogueRecord>,
}

impl BalanceConfig {
    pub fn new(
        fare_per_passenger_kilometre: MoneyPerKilometre,
        access_fee_per_train_kilometre: MoneyPerKilometre,
        starting_company_funds: Money,
        diesel_catalogue: Vec<DieselTrainCatalogueRecord>,
    ) -> Self {
        Self {
            fare_per_passenger_kilometre,
            access_fee_per_train_kilometre,
            starting_company_funds,
            diesel_catalogue,
        }
    }

    /// Creates the initial playtest balance. Every value in this method is a
    /// deliberately tunable provisional default, rather than a design rule.
    pub fn provisional() -> Self {
        // Tunable playtest rates, in cents per kilometre.
        let fare_per_passenger_kilometre =
            MoneyPerKilometre::new(20).expect("tunable fare rate must remain positive");
        let access_fee_per_train_kilometre =
            MoneyPerKilometre::new(12).expect("tunable access rate must remain positive");

        // Tunable playtest Company Funds, in cents.
        let starting_company_funds = Money::from_cents(500_000);

        // Tunable playtest diesel catalogue. The lower-priced Local has less
        // capacity and speed, but costs more fuel per kilometre than the
        // higher-priced Express.
        let diesel_catalogue = vec![
            DieselTrainCatalogueRecord::new(
                "Local 70",
                Money::from_cents(300_000),
                PassengerCapacity::new(70).expect("tunable capacity must remain positive"),
                SpeedMetresPerSecond::new(25).expect("tunable speed must remain positive"),
                MoneyPerKilometre::new(45).expect("tunable fuel rate must remain positive"),
            ),
            DieselTrainCatalogueRecord::new(
                "Express 120",
                Money::from_cents(500_000),
                PassengerCapacity::new(120).expect("tunable capacity must remain positive"),
                SpeedMetresPerSecond::new(33).expect("tunable speed must remain positive"),
                MoneyPerKilometre::new(30).expect("tunable fuel rate must remain positive"),
            ),
        ];

        Self::new(
            fare_per_passenger_kilometre,
            access_fee_per_train_kilometre,
            starting_company_funds,
            diesel_catalogue,
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

    pub fn diesel_catalogue(&self) -> &[DieselTrainCatalogueRecord] {
        &self.diesel_catalogue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_all_tunable_values_in_the_saved_configuration() {
        let rate = MoneyPerKilometre::new(25).unwrap();
        let stock = DieselTrainCatalogueRecord::new(
            "Test diesel",
            Money::from_cents(1),
            PassengerCapacity::new(1).unwrap(),
            SpeedMetresPerSecond::new(1).unwrap(),
            rate,
        );
        let balance = BalanceConfig::new(rate, rate, Money::from_cents(100), vec![stock.clone()]);

        assert_eq!(balance.fare_per_passenger_kilometre(), rate);
        assert_eq!(balance.access_fee_per_train_kilometre(), rate);
        assert_eq!(balance.starting_company_funds(), Money::from_cents(100));
        assert_eq!(balance.diesel_catalogue(), [stock]);
    }

    #[test]
    fn provisional_diesel_catalogue_has_the_intended_tradeoffs() {
        let balance = BalanceConfig::provisional();
        let [local, express] = balance.diesel_catalogue() else {
            panic!("the v0.1 catalogue must contain exactly two diesel Trains");
        };

        assert!(local.purchase_price() < express.purchase_price());
        assert!(local.passenger_capacity() < express.passenger_capacity());
        assert!(local.speed() < express.speed());
        assert!(local.fuel_cost_per_kilometre() > express.fuel_cost_per_kilometre());
    }

    #[test]
    fn provisional_starting_funds_cover_a_local_purchase_and_departure() {
        let balance = BalanceConfig::provisional();
        let local = &balance.diesel_catalogue()[0];
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
