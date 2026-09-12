//! Static Train catalogue shipped with RailQ.
//!
//! Train models are content, not save-game state. The embedded RON catalogue
//! is parsed once per process and owned Trains persist only a stable model ID
//! plus instance-specific state.

use std::{error::Error, fmt, sync::OnceLock};

use serde::Deserialize;

use crate::model::{
    Money, MoneyPerKilometre, PassengerCapacity, SpeedMetresPerSecond, Train, TrainModelId,
};

const EMBEDDED_TRAIN_CATALOGUE: &str = include_str!("../assets/trains.ron");

static TRAIN_CATALOGUE: OnceLock<TrainCatalogue> = OnceLock::new();

/// One immutable Train model available in the RailQ market.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrainModel {
    id: TrainModelId,
    name: String,
    purchase_price: Money,
    passenger_capacity: PassengerCapacity,
    speed: SpeedMetresPerSecond,
    fuel_cost_per_kilometre: MoneyPerKilometre,
}

impl TrainModel {
    pub fn id(&self) -> &TrainModelId {
        &self.id
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

/// Immutable Train models available to every game in this RailQ build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrainCatalogue {
    models: Vec<TrainModel>,
}

impl TrainCatalogue {
    /// Parses and validates a RON catalogue.
    pub fn from_ron(source: &str) -> Result<Self, CatalogueError> {
        let raw: Vec<RawTrainModel> =
            ron::from_str(source).map_err(|error| CatalogueError::Decode(error.to_string()))?;
        if raw.is_empty() {
            return Err(CatalogueError::Empty);
        }

        let mut models = Vec::with_capacity(raw.len());
        for record in raw {
            let id = TrainModelId::new(record.id);
            if id.as_str().trim().is_empty() {
                return Err(CatalogueError::InvalidField {
                    model_id: "<empty>".into(),
                    field: "id",
                });
            }
            if models.iter().any(|model: &TrainModel| model.id == id) {
                return Err(CatalogueError::DuplicateId(id.as_str().to_owned()));
            }
            if record.name.trim().is_empty() {
                return Err(CatalogueError::InvalidField {
                    model_id: id.as_str().to_owned(),
                    field: "name",
                });
            }
            if record.purchase_price_cents <= 0 {
                return Err(CatalogueError::InvalidField {
                    model_id: id.as_str().to_owned(),
                    field: "purchase_price_cents",
                });
            }
            let passenger_capacity = PassengerCapacity::new(record.passenger_capacity)
                .map_err(|_| CatalogueError::InvalidField {
                    model_id: id.as_str().to_owned(),
                    field: "passenger_capacity",
                })?;
            let speed = SpeedMetresPerSecond::new(record.speed_metres_per_second)
                .map_err(|_| CatalogueError::InvalidField {
                    model_id: id.as_str().to_owned(),
                    field: "speed_metres_per_second",
                })?;
            let fuel_cost_per_kilometre =
                MoneyPerKilometre::new(record.fuel_cost_cents_per_kilometre).map_err(|_| {
                    CatalogueError::InvalidField {
                        model_id: id.as_str().to_owned(),
                        field: "fuel_cost_cents_per_kilometre",
                    }
                })?;

            models.push(TrainModel {
                id,
                name: record.name,
                purchase_price: Money::from_cents(record.purchase_price_cents),
                passenger_capacity,
                speed,
                fuel_cost_per_kilometre,
            });
        }

        Ok(Self { models })
    }

    pub fn models(&self) -> &[TrainModel] {
        &self.models
    }

    pub fn get(&self, index: usize) -> Option<&TrainModel> {
        self.models.get(index)
    }

    pub fn by_id(&self, id: &TrainModelId) -> Option<&TrainModel> {
        self.models.iter().find(|model| model.id() == id)
    }
}

/// Returns RailQ's embedded Train catalogue, parsing it exactly once.
///
/// An invalid embedded catalogue is a developer/package error, so startup
/// fails loudly rather than allowing games with inconsistent Train data.
pub fn train_catalogue() -> &'static TrainCatalogue {
    TRAIN_CATALOGUE.get_or_init(|| {
        TrainCatalogue::from_ron(EMBEDDED_TRAIN_CATALOGUE)
            .expect("embedded assets/trains.ron must contain a valid Train catalogue")
    })
}

/// Resolves one owned Train's immutable model definition.
pub fn model_for_train(train: &Train) -> Option<&'static TrainModel> {
    train_catalogue().by_id(&train.model_id)
}

#[derive(Debug, Eq, PartialEq)]
pub enum CatalogueError {
    Decode(String),
    Empty,
    DuplicateId(String),
    InvalidField {
        model_id: String,
        field: &'static str,
    },
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "could not decode Train catalogue: {error}"),
            Self::Empty => write!(formatter, "Train catalogue must contain at least one model"),
            Self::DuplicateId(id) => write!(formatter, "duplicate Train model ID {id}"),
            Self::InvalidField { model_id, field } => {
                write!(formatter, "Train model {model_id} has invalid {field}")
            }
        }
    }
}

impl Error for CatalogueError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrainModel {
    id: String,
    name: String,
    purchase_price_cents: i64,
    passenger_capacity: i64,
    speed_metres_per_second: i64,
    fuel_cost_cents_per_kilometre: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalogue_has_stable_unique_models() {
        let catalogue = train_catalogue();
        assert_eq!(catalogue.models().len(), 2);
        assert_eq!(catalogue.models()[0].id().as_str(), "local-70");
        assert_eq!(catalogue.models()[1].id().as_str(), "express-120");
    }

    #[test]
    fn rejects_duplicate_model_ids() {
        let source = r#"[
            (id: "same", name: "A", purchase_price_cents: 1, passenger_capacity: 1, speed_metres_per_second: 1, fuel_cost_cents_per_kilometre: 1),
            (id: "same", name: "B", purchase_price_cents: 1, passenger_capacity: 1, speed_metres_per_second: 1, fuel_cost_cents_per_kilometre: 1),
        ]"#;
        assert_eq!(
            TrainCatalogue::from_ron(source),
            Err(CatalogueError::DuplicateId("same".into()))
        );
    }
}
