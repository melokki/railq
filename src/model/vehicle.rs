//! Rolling-stock and operator identity value objects.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

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

pub(super) fn evn_check_digit(first_eleven_digits: &str) -> Option<u8> {
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
