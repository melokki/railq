//! Tunable economic rules saved with each game.

use serde::{Deserialize, Serialize};

use crate::model::MoneyPerKilometre;

/// The tunable fare and operating rates for one game.
///
/// Rates are in cents per kilometre. Fare is charged once per passenger;
/// access and fuel are charged once per Train Journey.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BalanceConfig {
    fare_per_passenger_kilometre: MoneyPerKilometre,
    access_fee_per_train_kilometre: MoneyPerKilometre,
    fuel_cost_per_train_kilometre: MoneyPerKilometre,
}

impl BalanceConfig {
    pub const fn new(
        fare_per_passenger_kilometre: MoneyPerKilometre,
        access_fee_per_train_kilometre: MoneyPerKilometre,
        fuel_cost_per_train_kilometre: MoneyPerKilometre,
    ) -> Self {
        Self {
            fare_per_passenger_kilometre,
            access_fee_per_train_kilometre,
            fuel_cost_per_train_kilometre,
        }
    }

    pub const fn fare_per_passenger_kilometre(&self) -> MoneyPerKilometre {
        self.fare_per_passenger_kilometre
    }

    pub const fn access_fee_per_train_kilometre(&self) -> MoneyPerKilometre {
        self.access_fee_per_train_kilometre
    }

    pub const fn fuel_cost_per_train_kilometre(&self) -> MoneyPerKilometre {
        self.fuel_cost_per_train_kilometre
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_all_tunable_rates_in_the_saved_configuration() {
        let rate = MoneyPerKilometre::new(25).unwrap();
        let balance = BalanceConfig::new(rate, rate, rate);

        assert_eq!(balance.fare_per_passenger_kilometre(), rate);
        assert_eq!(balance.access_fee_per_train_kilometre(), rate);
        assert_eq!(balance.fuel_cost_per_train_kilometre(), rate);
    }
}
