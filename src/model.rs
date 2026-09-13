//! Core domain value types.
//!
//! All calculations use integer units. When a calculation has a fractional
//! result, RailQ rounds up: a partial cent or partial second is charged or
//! scheduled as one whole unit. This keeps quotes deterministic and never
//! understates a cost or Journey duration.

use std::{collections::BTreeMap, error::Error, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::balance::BalanceConfig;

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

/// Stable identity of one immutable Train model in the central catalogue.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TrainModelId(String);

impl TrainModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A 12-digit European Vehicle Number (EVN)-style identity.
///
/// RailQ follows the real EVN structure for tractive stock:
/// - digits 1–2: vehicle type code,
/// - digits 3–4: Region railway registration code,
/// - digits 5–8: RailQ model/class series code,
/// - digits 9–11: unit number within that model/class,
/// - digit 12: modulo-10 check digit.
///
/// The Region codes and model-series allocation are fictional RailQ data.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EuropeanVehicleNumber(String);

impl EuropeanVehicleNumber {
    pub const MAX_SERIES_CODE: u16 = 9_999;
    pub const MAX_UNIT_NUMBER: u16 = 999;

    /// Generates one EVN-style number using RailQ's model/class series plus a
    /// lifetime unit number within that model.
    pub fn generate(
        vehicle_type_code: u8,
        registration_code: u8,
        series_code: u16,
        unit_number: u16,
    ) -> Result<Self, EuropeanVehicleNumberError> {
        if !(90..=99).contains(&vehicle_type_code) {
            return Err(EuropeanVehicleNumberError::InvalidVehicleTypeCode);
        }
        if !(10..=99).contains(&registration_code) {
            return Err(EuropeanVehicleNumberError::InvalidRegistrationCode);
        }
        if series_code > Self::MAX_SERIES_CODE {
            return Err(EuropeanVehicleNumberError::InvalidSeriesCode);
        }
        if unit_number == 0 || unit_number > Self::MAX_UNIT_NUMBER {
            return Err(EuropeanVehicleNumberError::InvalidUnitNumber);
        }

        let base =
            format!("{vehicle_type_code:02}{registration_code:02}{series_code:04}{unit_number:03}");
        let check_digit =
            evn_check_digit(&base).ok_or(EuropeanVehicleNumberError::InvalidFormat)?;
        Ok(Self(format!("{base}{check_digit}")))
    }

    /// Parses and validates a complete 12-digit EVN.
    pub fn parse(value: &str) -> Result<Self, EuropeanVehicleNumberError> {
        if value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(EuropeanVehicleNumberError::InvalidFormat);
        }
        let vehicle_type_code = value[0..2]
            .parse::<u8>()
            .map_err(|_| EuropeanVehicleNumberError::InvalidFormat)?;
        let registration_code = value[2..4]
            .parse::<u8>()
            .map_err(|_| EuropeanVehicleNumberError::InvalidFormat)?;
        if !(90..=99).contains(&vehicle_type_code) {
            return Err(EuropeanVehicleNumberError::InvalidVehicleTypeCode);
        }
        if !(10..=99).contains(&registration_code) {
            return Err(EuropeanVehicleNumberError::InvalidRegistrationCode);
        }
        let unit_number = value[8..11]
            .parse::<u16>()
            .map_err(|_| EuropeanVehicleNumberError::InvalidFormat)?;
        if unit_number == 0 {
            return Err(EuropeanVehicleNumberError::InvalidUnitNumber);
        }
        let expected =
            evn_check_digit(&value[..11]).ok_or(EuropeanVehicleNumberError::InvalidFormat)?;
        let actual = value.as_bytes()[11] - b'0';
        if expected != actual {
            return Err(EuropeanVehicleNumberError::InvalidCheckDigit);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn vehicle_type_code(&self) -> u8 {
        self.0[0..2]
            .parse()
            .expect("validated EVN vehicle type code is numeric")
    }

    pub fn registration_code(&self) -> u8 {
        self.0[2..4]
            .parse()
            .expect("validated EVN registration code is numeric")
    }

    pub fn series_code(&self) -> u16 {
        self.0[4..8]
            .parse()
            .expect("validated EVN series code is numeric")
    }

    pub fn unit_number(&self) -> u16 {
        self.0[8..11]
            .parse()
            .expect("validated EVN unit number is numeric")
    }

    /// Formats the numeric EVN using the common 2-2-4-3-check grouping.
    pub fn formatted(&self) -> String {
        format!(
            "{} {} {} {}-{}",
            &self.0[0..2],
            &self.0[2..4],
            &self.0[4..8],
            &self.0[8..11],
            &self.0[11..12],
        )
    }

    /// Formats the full RailQ vehicle marking with Region mark and Company VKM.
    pub fn marking(&self, region_mark: &str, vkm: &VehicleKeeperMark) -> String {
        format!("{} {}-{}", self.formatted(), region_mark, vkm.as_str())
    }
}

fn evn_check_digit(first_eleven_digits: &str) -> Option<u8> {
    if first_eleven_digits.len() != 11
        || !first_eleven_digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let sum = first_eleven_digits
        .bytes()
        .enumerate()
        .map(|(index, byte)| {
            let digit = u16::from(byte - b'0');
            let factor = if index % 2 == 0 { 2 } else { 1 };
            let product = digit * factor;
            (product / 10) + (product % 10)
        })
        .sum::<u16>();

    Some(((10 - (sum % 10)) % 10) as u8)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EuropeanVehicleNumberError {
    InvalidFormat,
    InvalidVehicleTypeCode,
    InvalidRegistrationCode,
    InvalidSeriesCode,
    InvalidUnitNumber,
    InvalidCheckDigit,
}

impl fmt::Display for EuropeanVehicleNumberError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => write!(formatter, "EVN must contain exactly 12 digits"),
            Self::InvalidVehicleTypeCode => {
                write!(formatter, "EVN vehicle type code must be between 90 and 99")
            }
            Self::InvalidRegistrationCode => {
                write!(formatter, "EVN registration code must be between 10 and 99")
            }
            Self::InvalidSeriesCode => write!(
                formatter,
                "EVN series code must be between 0000 and {:04}",
                EuropeanVehicleNumber::MAX_SERIES_CODE
            ),
            Self::InvalidUnitNumber => write!(
                formatter,
                "EVN unit number must be between 001 and {:03}",
                EuropeanVehicleNumber::MAX_UNIT_NUMBER
            ),
            Self::InvalidCheckDigit => write!(formatter, "EVN check digit is invalid"),
        }
    }
}

impl Error for EuropeanVehicleNumberError {}

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
deserialize_validated_positive!(DistanceMetres, i64);
deserialize_validated_positive!(MoneyPerKilometre, i64);
deserialize_validated_positive!(PassengerArrivalRate, i64);

/// The complete mutable state of one RailQ game.
///
/// The Region owns public infrastructure through its Rail Authority. The
/// Player Company separately owns its Fleet and Passenger Services. Active
/// Journeys and origin-destination demand belong to the game because they
/// describe the current operating state rather than either owner's assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameState {
    /// The seed that generated this game's Region. It is saved so a loaded
    /// game never needs to regenerate its world.
    pub world_seed: u64,
    pub region: Region,
    pub player_company: PlayerCompany,
    pub origin_destination_demand: Vec<OriginDestinationDemand>,
    pub active_journeys: Vec<Journey>,
    pub financials: Financials,
    pub rules: GameRules,
    pub last_processed_at: UtcSeconds,
}

/// The fictional place in which a game takes place.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Region {
    pub name: String,
    /// Stable fictional railway registration identity used for vehicle numbering.
    #[serde(default)]
    pub railway_registration: RailwayRegistration,
    /// The total Population of every Settlement in this Region.
    pub population: u64,
    pub settlements: Vec<Settlement>,
    pub rail_authority: RailAuthority,
}

/// Stable fictional registration identity assigned when a Region is generated.
///
/// `numeric_code` is deliberately two digits so it can later occupy the
/// country-code position of RailQ's EVN-style vehicle numbers. `mark` is the
/// short alphabetic Region marking shown alongside that identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailwayRegistration {
    pub numeric_code: u8,
    pub mark: String,
}

impl Default for RailwayRegistration {
    fn default() -> Self {
        Self {
            numeric_code: 99,
            mark: "RQ".into(),
        }
    }
}

impl RailwayRegistration {
    pub fn display_code(&self) -> String {
        format!("{:02}", self.numeric_code)
    }
}

/// A populated place in a Region, with or without railway access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Settlement {
    pub id: SettlementId,
    pub name: String,
    pub population: u64,
}

/// The public owner of a Region's Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailAuthority {
    pub name: String,
    pub rail_network: RailNetwork,
}

/// The physical infrastructure owned by a Rail Authority.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailNetwork {
    pub rail_stations: Vec<RailStation>,
    pub rail_lines: Vec<RailLine>,
}

/// A facility providing one Settlement access to the Rail Network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailStation {
    pub id: RailStationId,
    pub settlement_id: SettlementId,
}

/// A physical connection between two Rail Stations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RailLine {
    pub id: RailLineId,
    pub first_station_id: RailStationId,
    pub second_station_id: RailStationId,
    pub distance: DistanceMetres,
}

/// A 2–5 letter Vehicle Keeper Mark (VKM) used to identify the Player Company.
///
/// RailQ treats the mark as a stable company identity that can later appear
/// alongside European-style vehicle numbers.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct VehicleKeeperMark(String);

impl VehicleKeeperMark {
    /// Parses and normalizes a player-entered VKM.
    pub fn parse(value: &str) -> Result<Self, VehicleKeeperMarkError> {
        let normalized = value.trim().to_ascii_uppercase();
        if !(2..=5).contains(&normalized.len()) {
            return Err(VehicleKeeperMarkError::InvalidLength);
        }
        if !normalized
            .chars()
            .all(|character| character.is_ascii_uppercase())
        {
            return Err(VehicleKeeperMarkError::InvalidCharacter);
        }
        Ok(Self(normalized))
    }

    /// Generates a deterministic default mark from a Player Company name.
    ///
    /// Multi-word names prefer initials (`One More Prime` -> `OMP`). A
    /// single-word ASCII name uses its first five letters. Names without
    /// enough ASCII letters fall back to a stable RailQ mark.
    pub fn generated_from_company_name(name: &str) -> Self {
        let words = name
            .split(|character: char| !character.is_ascii_alphabetic())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();

        let mut mark = String::new();
        if words.len() >= 2 {
            for word in words.iter().take(5) {
                if let Some(character) = word.chars().next() {
                    mark.push(character.to_ascii_uppercase());
                }
            }
        } else if let Some(word) = words.first() {
            for character in word.chars().take(5) {
                mark.push(character.to_ascii_uppercase());
            }
        }

        if mark.len() < 2 {
            for character in name
                .chars()
                .filter(|character| character.is_ascii_alphabetic())
            {
                if mark.len() >= 2 {
                    break;
                }
                let character = character.to_ascii_uppercase();
                if mark.chars().last() != Some(character) {
                    mark.push(character);
                }
            }
        }

        if mark.is_empty() {
            mark.push_str("RQ");
        } else if mark.len() == 1 {
            mark.push('X');
        }
        mark.truncate(5);
        Self(mark)
    }

    /// Returns the canonical uppercase mark.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VehicleKeeperMark {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Why a Player Company VKM cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VehicleKeeperMarkError {
    /// A VKM must contain between two and five letters.
    InvalidLength,
    /// A VKM may contain uppercase ASCII letters only.
    InvalidCharacter,
}

impl fmt::Display for VehicleKeeperMarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => write!(formatter, "VKM must contain 2 to 5 letters"),
            Self::InvalidCharacter => write!(formatter, "VKM may contain letters A-Z only"),
        }
    }
}

impl Error for VehicleKeeperMarkError {}

/// Optional player-facing name for one owned Train.
///
/// The official EVN remains the Train's permanent railway identity. A nickname
/// is deliberately separate so the player can rename a Train without changing
/// its official number or catalogue model.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct TrainNickname(String);

impl TrainNickname {
    pub const MAX_CHARACTERS: usize = 32;

    /// Parses a player-entered nickname after trimming surrounding whitespace.
    pub fn parse(value: &str) -> Result<Self, TrainNicknameError> {
        let normalized = value.trim();
        if normalized.is_empty() {
            return Err(TrainNicknameError::Empty);
        }
        if normalized.chars().count() > Self::MAX_CHARACTERS {
            return Err(TrainNicknameError::TooLong);
        }
        if normalized.chars().any(char::is_control) {
            return Err(TrainNicknameError::InvalidCharacter);
        }
        Ok(Self(normalized.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TrainNickname {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Why a Train nickname cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrainNicknameError {
    Empty,
    TooLong,
    InvalidCharacter,
}

impl fmt::Display for TrainNicknameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "Train nickname cannot be empty"),
            Self::TooLong => write!(
                formatter,
                "Train nickname must be at most {} characters",
                TrainNickname::MAX_CHARACTERS
            ),
            Self::InvalidCharacter => {
                write!(
                    formatter,
                    "Train nickname cannot contain control characters"
                )
            }
        }
    }
}

impl Error for TrainNicknameError {}

/// The passenger railway company controlled by the player.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlayerCompany {
    pub name: String,
    pub vehicle_keeper_mark: VehicleKeeperMark,
    pub funds: Money,
    pub fleet: Fleet,
    pub passenger_services: Vec<PassengerService>,
}

/// All Trains owned by the Player Company.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Fleet {
    pub trains: Vec<Train>,
    /// Next lifetime internal Train identity. Internal IDs are never reused.
    #[serde(default = "default_next_train_id")]
    pub next_train_id: u64,
    /// Next EVN unit number to allocate for each persistent Train model.
    ///
    /// Values are one-based; 1000 means the model's 001–999 allocation is
    /// exhausted. Keeping the counter after resale prevents EVN reuse.
    #[serde(default)]
    pub next_evn_unit_by_model: BTreeMap<TrainModelId, u16>,
}

const fn default_next_train_id() -> u64 {
    1
}

impl Default for Fleet {
    fn default() -> Self {
        Self {
            trains: Vec::new(),
            next_train_id: default_next_train_id(),
            next_evn_unit_by_model: BTreeMap::new(),
        }
    }
}

/// Passenger rolling stock owned by the Player Company.
///
/// Immutable technical specifications live in the central Train catalogue. An
/// owned Train persists only the stable catalogue model ID plus instance state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Train {
    pub id: TrainId,
    /// Permanent official vehicle number assigned when the Train joins the Fleet.
    pub evn: EuropeanVehicleNumber,
    /// Optional player-facing name. It never changes the official EVN.
    #[serde(default)]
    pub nickname: Option<TrainNickname>,
    pub status: TrainStatus,
    pub model_id: TrainModelId,
    /// The amount actually paid when this Train joined the Fleet.
    ///
    /// Resale is calculated from this historical purchase price, not the
    /// current catalogue price.
    pub original_purchase_price: Money,
}

/// The mutually exclusive operating status of a Train.
///
/// A Train is either ready at a Rail Station or travelling on one Journey;
/// the enum representation makes it impossible to represent both at once.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TrainStatus {
    Ready { at: RailStationId },
    Travelling { journey_id: JourneyId },
}

/// A directional passenger offering over an ordered set of stops and Rail Lines.
///
/// `stop_station_ids` contains only the stations at which the Service calls.
/// `rail_line_ids` contains the full physical path between those stops, so a
/// Service may pass through intermediate Rail Stations without stopping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PassengerService {
    pub id: ServiceId,
    pub name: String,
    pub stop_station_ids: Vec<RailStationId>,
    pub rail_line_ids: Vec<RailLineId>,
}

impl PassengerService {
    /// Returns the directional origin of this Service.
    pub fn origin_station_id(&self) -> Option<RailStationId> {
        self.stop_station_ids.first().copied()
    }

    /// Returns the directional destination of this Service.
    pub fn destination_station_id(&self) -> Option<RailStationId> {
        self.stop_station_ids.last().copied()
    }
}

/// Passengers currently aboard one active Journey, grouped by their final stop.
///
/// A group keeps its boarding origin and accepted fare so revenue can be
/// credited when those passengers actually alight, including after one or
/// more intermediate Service stops.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JourneyPassengerGroup {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub passengers: u32,
    pub fare: Money,
}

/// One Train run over a directional Passenger Service.
///
/// `current_stop_index` identifies the Service stop from which the current leg
/// departed. `arrives_at` is therefore the ETA of the next Service stop, not
/// necessarily the Service terminus. The same Journey ID remains active while
/// the Train calls at intermediate stops.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Journey {
    pub id: JourneyId,
    pub service_id: ServiceId,
    pub train_id: TrainId,
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    /// Total passenger boardings across the Service run so far.
    pub passengers_carried: u32,
    /// Through fare from the Service origin to terminus. Kept as a stable
    /// summary value; individual onboard groups may have shorter fares.
    pub fare: Money,
    /// Total booked revenue for all passenger groups boarded so far.
    pub operating_revenue: Money,
    /// Revenue already credited because passengers have reached their stops.
    #[serde(default)]
    pub credited_revenue: Money,
    /// The Rail Authority infrastructure charge for the complete Service run,
    /// paid at initial dispatch.
    pub infrastructure_access_fee: Money,
    /// Diesel fuel cost for the complete Service run, paid at initial dispatch.
    pub fuel_cost: Money,
    /// Index of the Service stop at which the current leg began.
    #[serde(default)]
    pub current_stop_index: usize,
    /// Passenger groups still aboard the Train.
    #[serde(default)]
    pub passenger_groups: Vec<JourneyPassengerGroup>,
    /// Departure time of the current leg.
    pub departed_at: UtcSeconds,
    /// Arrival time of the next Service stop.
    pub arrives_at: UtcSeconds,
}

impl Journey {
    /// Current onboard occupancy. This may be lower than `passengers_carried`
    /// because the latter counts every boarding over the full Service run.
    pub fn onboard_passengers(&self) -> u32 {
        self.passenger_groups
            .iter()
            .fold(0_u32, |total, group| total.saturating_add(group.passengers))
    }
}

/// Waiting passengers for one directional origin-destination market.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OriginDestinationDemand {
    pub origin_station_id: RailStationId,
    pub destination_station_id: RailStationId,
    pub waiting_passengers: u32,
    /// New Waiting Passengers generated per hour for this direction.
    pub passenger_arrival_rate_per_hour: PassengerArrivalRate,
    /// Passenger-seconds left over after the last whole-passenger update.
    ///
    /// This is always less than one hour while the pool is below its cap.
    /// It is cleared when the pool reaches the cap, so capped demand cannot
    /// become a hidden backlog.
    pub fractional_passenger_seconds: u64,
}

/// A positive directional Passenger Demand rate, in passengers per hour.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PassengerArrivalRate(u32);

impl PassengerArrivalRate {
    pub fn new(passengers_per_hour: i64) -> Result<Self, ValidationError> {
        let passengers_per_hour =
            u32::try_from(passengers_per_hour).map_err(|_| ValidationError::OutOfRange {
                unit: "passenger arrival rate per hour",
            })?;
        if passengers_per_hour == 0 {
            return Err(ValidationError::NonPositive {
                unit: "passenger arrival rate per hour",
            });
        }
        Ok(Self(passengers_per_hour))
    }

    pub const fn passengers_per_hour(self) -> u32 {
        self.0
    }
}

/// Cumulative financial data and receipts for the current game.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Financials {
    pub operating_revenue: Money,
    pub infrastructure_access_fees: Money,
    pub fuel_costs: Money,
    pub recent_journey_receipts: Vec<JourneyReceipt>,
}

/// The settled financial result and operating context of one Journey.
///
/// The optional context fields were added after the initial save format. They
/// default to `None` so version-1 RON saves containing older receipts remain
/// readable; newly settled Journeys always populate the complete snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JourneyReceipt {
    pub journey_id: JourneyId,
    pub revenue: Money,
    pub infrastructure_access_fee: Money,
    pub fuel_cost: Money,
    #[serde(default)]
    pub train_id: Option<TrainId>,
    #[serde(default)]
    pub train_model_name: Option<String>,
    #[serde(default)]
    pub origin_station_id: Option<RailStationId>,
    #[serde(default)]
    pub destination_station_id: Option<RailStationId>,
    #[serde(default)]
    pub passengers_carried: Option<u32>,
    #[serde(default)]
    pub passenger_capacity: Option<u32>,
    #[serde(default)]
    pub completed_at: Option<UtcSeconds>,
}

/// Per-save simulation rules. Static Train model definitions are build content
/// from the central catalogue and are intentionally not duplicated here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GameRules {
    pub balance: BalanceConfig,
    pub demand: DemandRules,
}

/// Tunable Passenger Demand rules saved with a game.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DemandRules {
    /// The time period of demand a directional pool can retain.
    pub cap_duration: DurationSeconds,
}

impl DemandRules {
    /// The initial playtest rule retains at most 24 hours of each directional
    /// Passenger Demand rate.
    pub const fn provisional() -> Self {
        Self {
            cap_duration: DurationSeconds::from_seconds(24 * 60 * 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{any::TypeId, collections::HashSet};

    use super::*;

    #[test]
    fn domain_ids_are_distinct_value_types() {
        let ids = [
            TypeId::of::<SettlementId>(),
            TypeId::of::<RailStationId>(),
            TypeId::of::<RailLineId>(),
            TypeId::of::<TrainId>(),
            TypeId::of::<ServiceId>(),
            TypeId::of::<JourneyId>(),
        ];
        assert_eq!(HashSet::from(ids).len(), ids.len());

        assert_eq!(SettlementId::new(7).get(), 7);
        assert_eq!(RailStationId::new(7).get(), 7);
        assert_eq!(RailLineId::new(7).get(), 7);
        assert_eq!(TrainId::new(7).get(), 7);
        assert_eq!(ServiceId::new(7).get(), 7);
        assert_eq!(JourneyId::new(7).get(), 7);
    }

    #[test]
    fn evn_check_digit_matches_the_standard_modulo_ten_example() {
        assert_eq!(evn_check_digit("31513320198"), Some(0));
    }

    #[test]
    fn generates_and_formats_a_dmu_vehicle_number() {
        let evn = EuropeanVehicleNumber::generate(95, 72, 70, 1).unwrap();
        assert_eq!(evn.as_str(), "957200700012");
        assert_eq!(evn.formatted(), "95 72 0070 001-2");
        assert_eq!(evn.vehicle_type_code(), 95);
        assert_eq!(evn.registration_code(), 72);
        assert_eq!(evn.series_code(), 70);
        assert_eq!(evn.unit_number(), 1);
        assert_eq!(EuropeanVehicleNumber::parse(evn.as_str()).unwrap(), evn);
    }

    #[test]
    fn train_nickname_trims_and_preserves_player_casing() {
        let nickname = TrainNickname::parse("  Little Runner  ").unwrap();
        assert_eq!(nickname.as_str(), "Little Runner");
    }

    #[test]
    fn train_nickname_rejects_empty_and_overlong_values() {
        assert_eq!(TrainNickname::parse("   "), Err(TrainNicknameError::Empty));
        assert_eq!(
            TrainNickname::parse(&"x".repeat(TrainNickname::MAX_CHARACTERS + 1)),
            Err(TrainNicknameError::TooLong)
        );
    }

    #[test]
    fn rejects_an_invalid_evn_check_digit() {
        assert_eq!(
            EuropeanVehicleNumber::parse("957200700013"),
            Err(EuropeanVehicleNumberError::InvalidCheckDigit)
        );
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

    #[test]
    fn vehicle_keeper_mark_generates_from_company_name_and_validates_edits() {
        assert_eq!(
            VehicleKeeperMark::generated_from_company_name("One More Prime").as_str(),
            "OMP"
        );
        assert_eq!(
            VehicleKeeperMark::generated_from_company_name("Northstar").as_str(),
            "NORTH"
        );
        assert_eq!(VehicleKeeperMark::parse(" omp ").unwrap().as_str(), "OMP");
        assert_eq!(
            VehicleKeeperMark::parse("O"),
            Err(VehicleKeeperMarkError::InvalidLength)
        );
        assert_eq!(
            VehicleKeeperMark::parse("O1P"),
            Err(VehicleKeeperMarkError::InvalidCharacter)
        );
    }

    #[test]
    fn a_small_valid_game_state_fixture_builds() {
        let settlement_id = SettlementId::new(1);
        let station_id = RailStationId::new(1);
        let train_id = TrainId::new(1);
        let rate = MoneyPerKilometre::new(1).unwrap();
        let state = GameState {
            world_seed: 0,
            region: Region {
                name: "Varelia".into(),
                railway_registration: RailwayRegistration {
                    numeric_code: 67,
                    mark: "VA".into(),
                },
                population: 1_000,
                settlements: vec![Settlement {
                    id: settlement_id,
                    name: "Alden".into(),
                    population: 1_000,
                }],
                rail_authority: RailAuthority {
                    name: "Varelia Rail Authority".into(),
                    rail_network: RailNetwork {
                        rail_stations: vec![RailStation {
                            id: station_id,
                            settlement_id,
                        }],
                        rail_lines: vec![],
                    },
                },
            },
            player_company: PlayerCompany {
                name: "Alden Passenger".into(),
                vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name(
                    "Alden Passenger",
                ),
                funds: Money::from_cents(10_000),
                fleet: Fleet {
                    trains: vec![Train {
                        id: train_id,
                        evn: EuropeanVehicleNumber::generate(95, 67, 701, 1).unwrap(),
                        nickname: None,
                        status: TrainStatus::Ready { at: station_id },
                        model_id: TrainModelId::new("helvetra-r70"),
                        original_purchase_price: Money::from_cents(5_000),
                    }],
                    next_train_id: train_id.get() + 1,
                    next_evn_unit_by_model: [(TrainModelId::new("helvetra-r70"), 2)]
                        .into_iter()
                        .collect(),
                },
                passenger_services: vec![],
            },
            origin_destination_demand: vec![],
            active_journeys: vec![],
            financials: Financials {
                operating_revenue: Money::ZERO,
                infrastructure_access_fees: Money::ZERO,
                fuel_costs: Money::ZERO,
                recent_journey_receipts: vec![],
            },
            rules: GameRules {
                balance: BalanceConfig::new(rate, rate, Money::from_cents(10_000)),
                demand: DemandRules::provisional(),
            },
            last_processed_at: UtcSeconds::from_unix_seconds(0),
        };

        assert_eq!(state.player_company.fleet.trains[0].id, train_id);
        assert_eq!(
            state.region.rail_authority.rail_network.rail_stations[0].id,
            station_id
        );
    }

    #[test]
    fn rail_authority_and_player_company_are_distinct_owners() {
        fn owns_network(_: &RailAuthority) {}
        fn owns_fleet_and_services(_: &PlayerCompany) {}

        let authority = RailAuthority {
            name: "Varelia Rail Authority".into(),
            rail_network: RailNetwork::default(),
        };
        let company = PlayerCompany {
            name: "Alden Passenger".into(),
            vehicle_keeper_mark: VehicleKeeperMark::generated_from_company_name("Alden Passenger"),
            funds: Money::ZERO,
            fleet: Fleet::default(),
            passenger_services: vec![],
        };

        owns_network(&authority);
        owns_fleet_and_services(&company);
    }

    #[test]
    fn a_train_has_exactly_one_operating_status() {
        let station_id = RailStationId::new(1);
        let ready = TrainStatus::Ready { at: station_id };
        let travelling = TrainStatus::Travelling {
            journey_id: JourneyId::new(1),
        };

        assert!(matches!(ready, TrainStatus::Ready { .. }));
        assert!(matches!(travelling, TrainStatus::Travelling { .. }));
    }

    #[test]
    fn rejects_non_positive_passenger_arrival_rates() {
        for rate in [-1, 0] {
            assert!(PassengerArrivalRate::new(rate).is_err());
        }
    }
}
