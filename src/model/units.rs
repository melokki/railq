//! Shared scalar value objects used across RailQ domains.
//!
//! These types keep unit conversion, validation, rounding, and overflow rules
//! close to the values they protect.

use std::{error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

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

impl Default for Money {
    fn default() -> Self {
        Self::ZERO
    }
}

/// A positive passenger capacity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

/// A positive infrastructure speed limit stored as whole kilometres per hour.
///
/// Rail infrastructure uses km/h because public line-speed upgrades are
/// expressed in familiar railway increments such as 70, 100, and 120 km/h.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SpeedKilometresPerHour(u16);

impl SpeedKilometresPerHour {
    pub fn new(kilometres_per_hour: i64) -> Result<Self, ValidationError> {
        let kilometres_per_hour =
            u16::try_from(kilometres_per_hour).map_err(|_| ValidationError::OutOfRange {
                unit: "speed in kilometres per hour",
            })?;
        if kilometres_per_hour == 0 {
            return Err(ValidationError::NonPositive {
                unit: "speed in kilometres per hour",
            });
        }
        Ok(Self(kilometres_per_hour))
    }

    pub const fn kilometres_per_hour(self) -> u16 {
        self.0
    }
}

/// Number of parallel running tracks on one physical Rail Line segment.
///
/// The first gameplay upgrades use one or two tracks, while the value object
/// deliberately supports larger future junction/corridor layouts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TrackCount(u8);

impl TrackCount {
    pub const SINGLE: Self = Self(1);
    pub const DOUBLE: Self = Self(2);

    pub fn new(tracks: i64) -> Result<Self, ValidationError> {
        let tracks = u8::try_from(tracks).map_err(|_| ValidationError::OutOfRange {
            unit: "track count",
        })?;
        if tracks == 0 {
            return Err(ValidationError::NonPositive {
                unit: "track count",
            });
        }
        Ok(Self(tracks))
    }

    pub const fn tracks(self) -> u8 {
        self.0
    }
}

impl Default for TrackCount {
    fn default() -> Self {
        Self::SINGLE
    }
}

/// Whether a physical Rail Line segment currently provides electric traction.
///
/// Voltage/current systems are intentionally deferred until they add useful
/// fleet gameplay; the infrastructure only needs electrified vs not yet.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Electrification {
    #[default]
    None,
    Electric,
}

/// Coarse physical difficulty used by future Authority project cost/duration
/// estimates. It is deliberately not a terrain simulation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ConstructionDifficulty {
    Low,
    #[default]
    Moderate,
    High,
}

/// A positive physical distance stored as integer metres.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

    /// Calculates a Journey duration at a Train speed, rounding a partial second up.
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

    /// Calculates Journey time while respecting both Train and infrastructure speeds.
    ///
    /// Train models store metres per second while Rail Lines store familiar railway
    /// limits in kilometres per hour. Computing both durations independently avoids
    /// lossy integer conversion between those units; the slower capability is simply
    /// the one that produces the longer duration.
    pub fn journey_duration_with_speed_limit(
        self,
        train_speed: SpeedMetresPerSecond,
        speed_limit: SpeedKilometresPerHour,
    ) -> Result<DurationSeconds, CalculationError> {
        let train_duration = self.journey_duration(train_speed)?;

        let numerator = self
            .0
            .checked_mul(3_600)
            .ok_or(CalculationError::Overflow {
                operation: "speed-limited journey duration",
            })?;
        let denominator =
            u64::from(speed_limit.0)
                .checked_mul(1_000)
                .ok_or(CalculationError::Overflow {
                    operation: "speed-limited journey duration",
                })?;
        let seconds = numerator / denominator;
        let has_fractional_second = numerator % denominator != 0;
        let seconds = if has_fractional_second {
            seconds.checked_add(1).ok_or(CalculationError::Overflow {
                operation: "speed-limited journey duration",
            })?
        } else {
            seconds
        };
        let infrastructure_duration = DurationSeconds(seconds);

        Ok(train_duration.max(infrastructure_duration))
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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = <$primitive>::try_from(self.0)
                    .expect("validated positive value fits its serialized integer type");
                value.serialize(serializer)
            }
        }

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
deserialize_validated_positive!(SpeedKilometresPerHour, i64);
deserialize_validated_positive!(TrackCount, i64);
deserialize_validated_positive!(DistanceMetres, i64);
deserialize_validated_positive!(MoneyPerKilometre, i64);
