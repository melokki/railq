//! Stable identities used across RailQ domain modules.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

macro_rules! domain_id {
    ($name:ident, $description:literal, $namespace:expr) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a fresh UUID v4 identity for a new persisted entity.
            pub fn new_v4() -> Self {
                Self(Uuid::new_v4())
            }

            /// Creates a UUID v4 while preserving a small numeric suffix for
            /// compact player-facing labels. The UUID remains the persisted
            /// identity; the suffix is display metadata only.
            pub fn new_v4_with_suffix(suffix: u64) -> Self {
                let random = Uuid::new_v4().as_u128();
                let payload =
                    (random & !((1_u128 << 62) - 1)) | u128::from(suffix & ((1_u64 << 62) - 1));
                Self::from_v4_bits(payload)
            }

            /// Creates a UUID identity from an already validated UUID.
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Deterministic UUID-v4-shaped constructor used by seeded world
            /// generation and compact test fixtures.
            ///
            /// Production entities that are not world-seeded should use
            /// [`Self::new_v4`] instead.
            pub const fn new(value: u64) -> Self {
                Self(Uuid::from_u128(
                    (($namespace as u128) << 96) | (4_u128 << 76) | (2_u128 << 62) | value as u128,
                ))
            }

            /// Creates a UUID v4 identity from 122 random payload bits.
            /// Version and RFC 4122 variant bits are normalized here so seeded
            /// world generation remains reproducible while persisted IDs are
            /// still UUID v4 values.
            pub const fn from_v4_bits(value: u128) -> Self {
                let value = (value & !(0xf_u128 << 76) & !(0x3_u128 << 62))
                    | (4_u128 << 76)
                    | (2_u128 << 62);
                Self(Uuid::from_u128(value))
            }

            /// Returns the UUID stored by this identity.
            pub const fn uuid(self) -> Uuid {
                self.0
            }

            /// Returns whether this identity uses the UUID v4 + RFC 4122 variant layout.
            pub const fn is_v4(self) -> bool {
                let value = self.0.as_u128();
                ((value >> 76) & 0x0f) == 4 && ((value >> 62) & 0x03) == 2
            }

            /// Returns a compact numeric suffix used only by legacy UI labels
            /// and deterministic fixtures. Persistence must use [`Self::uuid`].
            pub const fn get(self) -> u64 {
                (self.0.as_u128() & ((1_u128 << 62) - 1)) as u64
            }

            pub fn parse(value: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_newtype_struct(stringify!($name), &self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct DomainIdVisitor;

                impl<'de> de::Visitor<'de> for DomainIdVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str("a UUID or legacy positive integer ID")
                    }

                    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        Ok($name::new(value))
                    }

                    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        let value = u64::try_from(value)
                            .map_err(|_| E::custom("legacy ID must be non-negative"))?;
                        Ok($name::new(value))
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        $name::parse(value).map_err(E::custom)
                    }

                    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        self.visit_str(&value)
                    }

                    fn visit_newtype_struct<D2>(
                        self,
                        deserializer: D2,
                    ) -> Result<Self::Value, D2::Error>
                    where
                        D2: Deserializer<'de>,
                    {
                        deserializer.deserialize_any(self)
                    }
                }

                deserializer.deserialize_newtype_struct(stringify!($name), DomainIdVisitor)
            }
        }
    };
}

domain_id!(SettlementId, "The identity of a Settlement in a Region.", 1);
domain_id!(
    RailStationId,
    "The identity of a Rail Station in the Rail Network.",
    2
);
domain_id!(
    RailLineId,
    "The stable identity of one physical Rail Line segment in the Rail Network.",
    3
);
domain_id!(
    TrainId,
    "The identity of a Train owned by the Player Company.",
    5
);
domain_id!(
    ServiceId,
    "The identity of a Passenger Service owned by the Player Company.",
    6
);
domain_id!(JourneyId, "The identity of one physical Train movement.", 7);
domain_id!(
    InfrastructureProjectId,
    "The identity of one Rail Authority infrastructure project.",
    4
);

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
