//! Current SQLite schema definition.
//!
//! Migration logic remains separate so creating a fresh save and upgrading an
//! existing save can evolve independently.

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS game_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    world_seed TEXT NOT NULL,
    last_processed_at INTEGER NOT NULL,
    bulletin_seen_count INTEGER NOT NULL DEFAULT 0 CHECK (bulletin_seen_count >= 0)
);
CREATE TABLE IF NOT EXISTS region (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    registration_code INTEGER NOT NULL CHECK (registration_code BETWEEN 10 AND 99),
    registration_mark TEXT NOT NULL,
    population INTEGER NOT NULL,
    rail_authority_name TEXT NOT NULL,
    rail_authority_construction_capacity INTEGER NOT NULL DEFAULT 1 CHECK (rail_authority_construction_capacity > 0)
);
CREATE TABLE IF NOT EXISTS bulletin_entries (
    sequence INTEGER PRIMARY KEY,
    occurred_at INTEGER NOT NULL,
    category TEXT NOT NULL CHECK (category IN ('local', 'authority', 'construction', 'network')),
    headline TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rail_authority_finances (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    treasury_cents INTEGER NOT NULL CHECK (treasury_cents >= 0),
    maintenance_reserve_cents INTEGER NOT NULL CHECK (maintenance_reserve_cents >= 0),
    committed_investment_cents INTEGER NOT NULL CHECK (committed_investment_cents >= 0),
    carried_over_funds_cents INTEGER NOT NULL CHECK (carried_over_funds_cents >= 0),
    regional_public_allocation_cents INTEGER NOT NULL CHECK (regional_public_allocation_cents >= 0),
    infrastructure_access_fee_revenue_cents INTEGER NOT NULL CHECK (infrastructure_access_fee_revenue_cents >= 0),
    next_fiscal_period_at INTEGER
);
CREATE TABLE IF NOT EXISTS settlements (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    population INTEGER NOT NULL,
    world_x INTEGER NOT NULL,
    world_y INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rail_stations (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    settlement_id TEXT NOT NULL REFERENCES settlements(id)
);
CREATE TABLE IF NOT EXISTS rail_lines (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    first_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    second_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    distance_metres INTEGER NOT NULL,
    speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
    track_count INTEGER NOT NULL CHECK (track_count > 0),
    electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
    construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high'))
);
CREATE TABLE IF NOT EXISTS infrastructure_projects (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('new_line', 'speed_upgrade', 'double_tracking', 'electrification', 'renewal', 'station_upgrade')),
    status TEXT NOT NULL CHECK (status IN ('requested', 'under_review', 'proposed', 'approved', 'deferred', 'rejected', 'funding', 'scheduled', 'construction', 'open', 'cancelled')),
    estimated_cost_cents INTEGER NOT NULL DEFAULT 0 CHECK (estimated_cost_cents >= 0),
    authority_committed_cents INTEGER NOT NULL DEFAULT 0 CHECK (authority_committed_cents >= 0),
    operator_contributed_cents INTEGER NOT NULL DEFAULT 0 CHECK (operator_contributed_cents >= 0),
    access_fee_discount_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (access_fee_discount_basis_points BETWEEN 0 AND 10000),
    access_fee_discount_expires_at INTEGER,
    requested_at INTEGER NOT NULL,
    review_started_at INTEGER,
    proposed_at INTEGER,
    approved_at INTEGER,
    funding_completed_at INTEGER,
    scheduled_start_at INTEGER,
    construction_started_at INTEGER,
    planned_completion_at INTEGER,
    completed_at INTEGER,
    deferred_at INTEGER,
    cancelled_at INTEGER,
    reconsideration_count INTEGER NOT NULL DEFAULT 0 CHECK (reconsideration_count BETWEEN 0 AND 255),
    target_speed_limit_kmh INTEGER CHECK (target_speed_limit_kmh IS NULL OR target_speed_limit_kmh > 0),
    target_track_count INTEGER CHECK (target_track_count IS NULL OR target_track_count > 0)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_lines (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_rail_stations (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_stations (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id TEXT NOT NULL UNIQUE,
    settlement_id TEXT NOT NULL REFERENCES settlements(id),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS infrastructure_project_planned_lines (
    project_id TEXT NOT NULL REFERENCES infrastructure_projects(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    line_id TEXT NOT NULL UNIQUE,
    first_station_id TEXT NOT NULL,
    second_station_id TEXT NOT NULL,
    distance_metres INTEGER NOT NULL CHECK (distance_metres > 0),
    speed_limit_kmh INTEGER NOT NULL CHECK (speed_limit_kmh > 0),
    track_count INTEGER NOT NULL CHECK (track_count > 0),
    electrification TEXT NOT NULL CHECK (electrification IN ('none', 'electric')),
    construction_difficulty TEXT NOT NULL CHECK (construction_difficulty IN ('low', 'moderate', 'high')),
    PRIMARY KEY (project_id, sequence)
);
CREATE TABLE IF NOT EXISTS company (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    name TEXT NOT NULL,
    vkm TEXT NOT NULL CHECK (length(vkm) BETWEEN 2 AND 5) CHECK (vkm NOT GLOB '*[^A-Z]*'),
    funds_cents INTEGER NOT NULL,
    next_train_display_number INTEGER NOT NULL CHECK (next_train_display_number > 0)
);
CREATE TABLE IF NOT EXISTS trains (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    evn TEXT NOT NULL UNIQUE CHECK (length(evn) = 12) CHECK (evn NOT GLOB '*[^0-9]*'),
    nickname TEXT CHECK (nickname IS NULL OR length(trim(nickname)) BETWEEN 1 AND 32),
    status_kind TEXT NOT NULL CHECK (status_kind IN ('ready', 'travelling')),
    status_ref_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    original_purchase_price_cents INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS train_model_sequences (
    model_id TEXT PRIMARY KEY,
    next_unit_number INTEGER NOT NULL CHECK (next_unit_number BETWEEN 1 AND 1000)
);
CREATE TABLE IF NOT EXISTS passenger_services (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    custom_name TEXT CHECK (custom_name IS NULL OR length(trim(custom_name)) BETWEEN 1 AND 32),
    direction_mode TEXT NOT NULL DEFAULT 'both'
        CHECK (direction_mode IN ('both', 'forward')),
    forward_train_number INTEGER NOT NULL CHECK (forward_train_number >= 100),
    reverse_train_number INTEGER CHECK (reverse_train_number >= 100),
    CHECK (
        (direction_mode = 'both' AND reverse_train_number IS NOT NULL) OR
        (direction_mode = 'forward' AND reverse_train_number IS NULL)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS passenger_services_forward_train_number_idx
    ON passenger_services(forward_train_number);
CREATE UNIQUE INDEX IF NOT EXISTS passenger_services_reverse_train_number_idx
    ON passenger_services(reverse_train_number)
    WHERE reverse_train_number IS NOT NULL;
CREATE TABLE IF NOT EXISTS train_service_assignments (
    train_id TEXT PRIMARY KEY REFERENCES trains(id) ON DELETE CASCADE,
    service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS train_service_assignments_service_idx
    ON train_service_assignments(service_id);
CREATE TABLE IF NOT EXISTS service_stops (
    service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    station_id TEXT NOT NULL REFERENCES rail_stations(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS service_lines (
    service_id TEXT NOT NULL REFERENCES passenger_services(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    rail_line_id TEXT NOT NULL REFERENCES rail_lines(id),
    PRIMARY KEY (service_id, sequence)
);
CREATE TABLE IF NOT EXISTS origin_destination_demand (
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    sequence INTEGER NOT NULL UNIQUE,
    waiting_passengers INTEGER NOT NULL,
    market_maturity_basis_points INTEGER NOT NULL DEFAULT 10000
        CHECK (market_maturity_basis_points BETWEEN 0 AND 10000),
    passenger_arrival_rate_per_hour INTEGER NOT NULL,
    fractional_passenger_seconds INTEGER NOT NULL,
    PRIMARY KEY (origin_station_id, destination_station_id)
);
CREATE TABLE IF NOT EXISTS active_journeys (
    id TEXT PRIMARY KEY,
    sequence INTEGER NOT NULL UNIQUE,
    purpose TEXT NOT NULL DEFAULT 'revenue' CHECK (purpose IN ('revenue', 'positioning')),
    service_id TEXT NOT NULL REFERENCES passenger_services(id),
    train_id TEXT NOT NULL REFERENCES trains(id),
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    passengers_carried INTEGER NOT NULL,
    fare_rate_cents_per_passenger_km INTEGER NOT NULL CHECK (fare_rate_cents_per_passenger_km > 0),
    fare_cents INTEGER NOT NULL,
    operating_revenue_cents INTEGER NOT NULL,
    credited_revenue_cents INTEGER NOT NULL,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    current_stop_index INTEGER NOT NULL,
    started_at INTEGER,
    departed_at INTEGER NOT NULL,
    arrives_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journey_passenger_groups (
    journey_id TEXT NOT NULL REFERENCES active_journeys(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    origin_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    destination_station_id TEXT NOT NULL REFERENCES rail_stations(id),
    passengers INTEGER NOT NULL,
    fare_cents INTEGER NOT NULL,
    PRIMARY KEY (journey_id, sequence)
);
CREATE TABLE IF NOT EXISTS financials (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    operating_revenue_cents INTEGER NOT NULL,
    infrastructure_access_fees_cents INTEGER NOT NULL,
    fuel_costs_cents INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journey_receipts (
    journey_id TEXT PRIMARY KEY,
    revenue_cents INTEGER NOT NULL,
    infrastructure_access_fee_cents INTEGER NOT NULL,
    fuel_cost_cents INTEGER NOT NULL,
    train_id TEXT,
    train_model_name TEXT,
    origin_station_id TEXT,
    destination_station_id TEXT,
    passengers_carried INTEGER,
    passenger_capacity INTEGER,
    completed_at INTEGER,
    service_id TEXT,
    service_code TEXT,
    purpose TEXT CHECK (purpose IS NULL OR purpose IN ('revenue', 'positioning')),
    departed_at INTEGER
);
CREATE TABLE IF NOT EXISTS game_rules (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    fare_cents_per_passenger_km INTEGER NOT NULL,
    access_fee_cents_per_train_km INTEGER NOT NULL,
    starting_company_funds_cents INTEGER NOT NULL,
    demand_cap_seconds INTEGER NOT NULL,
    authority_request_queue_seconds INTEGER NOT NULL,
    authority_review_seconds INTEGER NOT NULL,
    authority_proposal_seconds INTEGER NOT NULL,
    authority_request_cooldown_seconds INTEGER NOT NULL,
    authority_deferred_reconsideration_seconds INTEGER NOT NULL,
    authority_mobilisation_seconds INTEGER NOT NULL,
    authority_new_line_base_construction_seconds INTEGER NOT NULL,
    authority_low_difficulty_seconds_per_km INTEGER NOT NULL,
    authority_moderate_difficulty_seconds_per_km INTEGER NOT NULL,
    authority_high_difficulty_seconds_per_km INTEGER NOT NULL,
    authority_max_active_expansion_projects INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_active_journeys_arrival ON active_journeys(arrives_at);
CREATE INDEX IF NOT EXISTS idx_receipts_completed_at ON journey_receipts(completed_at);
"#;
