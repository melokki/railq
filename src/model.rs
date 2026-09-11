//! Core domain value types.
//!
//! All calculations use integer units. When a calculation has a fractional
//! result, RailQ rounds up: a partial cent or partial second is charged or
//! scheduled as one whole unit. This keeps quotes deterministic and never
//! understates a cost or Journey duration.

use std::{error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, de};

/// An input that violates a value object's invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// The value must be greater than zero.
    NonPositive { unit: &'static str },
    /// The value cannot fit the storage unit for this value object.
    OutOfRange { unit: &'static str },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositive { unit } => write!(formatter, "{unit} must be greater than zero"),
            Self::OutOfRange { unit } => write!(formatter, "{unit} is outside the supported range"),
        }
    }
}

impl Error for ValidationError {}

/// A checked calculation that cannot be represented in its result unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalculationError {
    Overflow { operation: &'static str },
}

impl fmt::Display for CalculationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow { operation } => {
                write!(formatter, "overflow while calculating {operation}")
            }
        }
    }
}

impl Error for CalculationError {}

macro_rules! domain_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name(u64);

        impl $name {
            /// Creates an ID allocated by its owning collection.
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Returns the stored identifier value.
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

domain_id!(SettlementId, "The identity of a Settlement in a Region.");
domain_id!(
    RailStationId,
    "The identity of a Rail Station in the Rail Network."
);
domain_id!(
    RailLineId,
    "The identity of a Rail Line in the Rail Network."
);
domain_id!(
    TrainId,
    "The identity of a Train owned by the Player Company."
);
domain_id!(
    ServiceId,
    "The identity of a Passenger Service owned by the Player Company."
);
domain_id!(JourneyId, "The identity of one physical Train movement.");

/// A signed amount of money stored exactly as integer cents.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);

    /// Creates a money amount from its exact cent value.
    pub const fn from_cents(cents: i64) -> Self {
        Self(cents)
    }

    /// Returns the exact cent value.
    pub const fn cents(self) -> i64 {
        self.0
    }

    /// Adds two money amounts without silently wrapping.
    pub fn checked_add(self, other: Self) -> Result<Self, CalculationError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money addition",
            })
    }

    /// Subtracts two money amounts without silently wrapping.
    pub fn checked_sub(self, other: Self) -> Result<Self, CalculationError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money subtraction",
            })
    }

    /// Multiplies a money amount by a count without silently wrapping.
    pub fn checked_mul(self, count: u64) -> Result<Self, CalculationError> {
        let count = i64::try_from(count).map_err(|_| CalculationError::Overflow {
            operation: "money multiplication",
        })?;
        self.0
            .checked_mul(count)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "money multiplication",
            })
    }
}

/// A positive passenger capacity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PassengerCapacity(u32);

impl PassengerCapacity {
    pub fn new(passengers: i64) -> Result<Self, ValidationError> {
        let passengers = u32::try_from(passengers).map_err(|_| ValidationError::OutOfRange {
            unit: "passenger capacity",
        })?;
        if passengers == 0 {
            return Err(ValidationError::NonPositive {
                unit: "passenger capacity",
            });
        }
        Ok(Self(passengers))
    }

    pub const fn passengers(self) -> u32 {
        self.0
    }
}

/// A positive Train speed stored as metres per second.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SpeedMetresPerSecond(u64);

impl SpeedMetresPerSecond {
    pub fn new(metres_per_second: i64) -> Result<Self, ValidationError> {
        let metres_per_second =
            u64::try_from(metres_per_second).map_err(|_| ValidationError::OutOfRange {
                unit: "speed in metres per second",
            })?;
        if metres_per_second == 0 {
            return Err(ValidationError::NonPositive {
                unit: "speed in metres per second",
            });
        }
        Ok(Self(metres_per_second))
    }

    pub const fn metres_per_second(self) -> u64 {
        self.0
    }
}

/// A positive physical distance stored as integer metres.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DistanceMetres(u64);

impl DistanceMetres {
    pub fn new(metres: i64) -> Result<Self, ValidationError> {
        let metres = u64::try_from(metres).map_err(|_| ValidationError::OutOfRange {
            unit: "distance in metres",
        })?;
        if metres == 0 {
            return Err(ValidationError::NonPositive {
                unit: "distance in metres",
            });
        }
        Ok(Self(metres))
    }

    pub const fn metres(self) -> u64 {
        self.0
    }

    /// Calculates a Journey duration, rounding a partial second up.
    pub fn journey_duration(
        self,
        speed: SpeedMetresPerSecond,
    ) -> Result<DurationSeconds, CalculationError> {
        let seconds = self.0 / speed.0;
        let has_fractional_second = self.0 % speed.0 != 0;
        let seconds = if has_fractional_second {
            seconds.checked_add(1).ok_or(CalculationError::Overflow {
                operation: "journey duration",
            })?
        } else {
            seconds
        };
        Ok(DurationSeconds(seconds))
    }
}

/// A non-negative elapsed duration stored as whole seconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DurationSeconds(u64);

impl DurationSeconds {
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    pub const fn seconds(self) -> u64 {
        self.0
    }
}

/// A UTC Unix timestamp stored as whole seconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct UtcSeconds(i64);

impl UtcSeconds {
    pub const fn from_unix_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    pub const fn unix_seconds(self) -> i64 {
        self.0
    }

    /// Advances a timestamp without silently wrapping.
    pub fn checked_add(self, duration: DurationSeconds) -> Result<Self, CalculationError> {
        let duration = i64::try_from(duration.0).map_err(|_| CalculationError::Overflow {
            operation: "UTC timestamp addition",
        })?;
        self.0
            .checked_add(duration)
            .map(Self)
            .ok_or(CalculationError::Overflow {
                operation: "UTC timestamp addition",
            })
    }
}

/// A positive monetary rate in cents per kilometre.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct MoneyPerKilometre(u64);

impl MoneyPerKilometre {
    pub fn new(cents_per_kilometre: i64) -> Result<Self, ValidationError> {
        let cents_per_kilometre =
            u64::try_from(cents_per_kilometre).map_err(|_| ValidationError::OutOfRange {
                unit: "money rate in cents per kilometre",
            })?;
        if cents_per_kilometre == 0 {
            return Err(ValidationError::NonPositive {
                unit: "money rate in cents per kilometre",
            });
        }
        Ok(Self(cents_per_kilometre))
    }

    pub const fn cents_per_kilometre(self) -> u64 {
        self.0
    }

    /// Charges for a distance, rounding a partial cent up.
    pub fn checked_charge(self, distance: DistanceMetres) -> Result<Money, CalculationError> {
        const METRES_PER_KILOMETRE: u64 = 1_000;

        let cent_metres = self
            .0
            .checked_mul(distance.0)
            .ok_or(CalculationError::Overflow {
                operation: "distance rate",
            })?;
        let cents = cent_metres / METRES_PER_KILOMETRE;
        let cents = if cent_metres % METRES_PER_KILOMETRE != 0 {
            cents.checked_add(1).ok_or(CalculationError::Overflow {
                operation: "distance rate rounding",
            })?
        } else {
            cents
        };
        let cents = i64::try_from(cents).map_err(|_| CalculationError::Overflow {
            operation: "distance rate money conversion",
        })?;
        Ok(Money::from_cents(cents))
    }
}

macro_rules! deserialize_validated_positive {
    ($name:ident, $primitive:ty) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = <$primitive>::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

deserialize_validated_positive!(PassengerCapacity, i64);
deserialize_validated_positive!(SpeedMetresPerSecond, i64);
deserialize_validated_positive!(DistanceMetres, i64);
deserialize_validated_positive!(MoneyPerKilometre, i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_ids_are_distinct_value_types() {
        assert_eq!(SettlementId::new(7).get(), 7);
        assert_eq!(RailStationId::new(7).get(), 7);
        assert_eq!(RailLineId::new(7).get(), 7);
        assert_eq!(TrainId::new(7).get(), 7);
        assert_eq!(ServiceId::new(7).get(), 7);
        assert_eq!(JourneyId::new(7).get(), 7);
    }

    #[test]
    fn rejects_non_positive_domain_values() {
        for value in [-1, 0] {
            assert!(matches!(
                PassengerCapacity::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                SpeedMetresPerSecond::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                DistanceMetres::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
            assert!(matches!(
                MoneyPerKilometre::new(value),
                Err(ValidationError::NonPositive { .. }) | Err(ValidationError::OutOfRange { .. })
            ));
        }
    }

    #[test]
    fn rounds_partial_seconds_and_cents_up() {
        let speed = SpeedMetresPerSecond::new(1_000).unwrap();
        assert_eq!(
            DistanceMetres::new(1_000)
                .unwrap()
                .journey_duration(speed)
                .unwrap(),
            DurationSeconds::from_seconds(1)
        );
        assert_eq!(
            DistanceMetres::new(1_001)
                .unwrap()
                .journey_duration(speed)
                .unwrap(),
            DurationSeconds::from_seconds(2)
        );

        let rate = MoneyPerKilometre::new(1).unwrap();
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1_000).unwrap())
                .unwrap(),
            Money::from_cents(1)
        );
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1).unwrap())
                .unwrap(),
            Money::from_cents(1)
        );
        assert_eq!(
            rate.checked_charge(DistanceMetres::new(1_001).unwrap())
                .unwrap(),
            Money::from_cents(2)
        );
    }

    #[test]
    fn reports_overflow_boundaries() {
        assert_eq!(
            Money::from_cents(i64::MAX).checked_add(Money::from_cents(1)),
            Err(CalculationError::Overflow {
                operation: "money addition"
            })
        );
        assert_eq!(
            Money::from_cents(i64::MIN).checked_sub(Money::from_cents(1)),
            Err(CalculationError::Overflow {
                operation: "money subtraction"
            })
        );
        assert_eq!(
            MoneyPerKilometre::new(i64::MAX)
                .unwrap()
                .checked_charge(DistanceMetres::new(i64::MAX).unwrap()),
            Err(CalculationError::Overflow {
                operation: "distance rate"
            })
        );
        assert_eq!(
            UtcSeconds::from_unix_seconds(i64::MAX).checked_add(DurationSeconds::from_seconds(1)),
            Err(CalculationError::Overflow {
                operation: "UTC timestamp addition"
            })
        );
    }
}
