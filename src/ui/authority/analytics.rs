//! Presentation-only Authority dashboard analytics.
//!
//! Simulation statuses remain authoritative. This module groups them into a
//! smaller programme lifecycle for dashboard summaries without changing game
//! state or lifecycle rules.

use crate::model::{
    GameState, InfrastructureProjectId, InfrastructureProjectStatus, Money, UtcSeconds,
};

/// Player-facing lifecycle used by the Authority dashboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProgrammeStage {
    Planning,
    Funding,
    Queued,
    Building,
    Open,
    Deferred,
    Closed,
}

impl ProgrammeStage {
    pub(super) const fn from_status(status: InfrastructureProjectStatus) -> Self {
        match status {
            InfrastructureProjectStatus::Requested
            | InfrastructureProjectStatus::UnderReview
            | InfrastructureProjectStatus::Proposed
            | InfrastructureProjectStatus::Approved => Self::Planning,
            InfrastructureProjectStatus::Funding => Self::Funding,
            InfrastructureProjectStatus::Scheduled => Self::Queued,
            InfrastructureProjectStatus::Construction => Self::Building,
            InfrastructureProjectStatus::Open => Self::Open,
            InfrastructureProjectStatus::Deferred => Self::Deferred,
            InfrastructureProjectStatus::Rejected | InfrastructureProjectStatus::Cancelled => {
                Self::Closed
            }
        }
    }
}

/// Project counts grouped by the presentation lifecycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ProgrammeCounts {
    pub(super) planning: usize,
    pub(super) funding: usize,
    pub(super) queued: usize,
    pub(super) building: usize,
    pub(super) open: usize,
    pub(super) deferred: usize,
}

impl ProgrammeCounts {
    pub(super) fn from_state(state: &GameState) -> Self {
        let mut counts = Self::default();
        for project in &state.region.rail_authority.infrastructure_projects {
            match ProgrammeStage::from_status(project.status) {
                ProgrammeStage::Planning => counts.planning += 1,
                ProgrammeStage::Funding => counts.funding += 1,
                ProgrammeStage::Queued => counts.queued += 1,
                ProgrammeStage::Building => counts.building += 1,
                ProgrammeStage::Open => counts.open += 1,
                ProgrammeStage::Deferred => counts.deferred += 1,
                ProgrammeStage::Closed => {}
            }
        }
        counts
    }

    pub(super) const fn pipeline(self) -> usize {
        self.planning + self.funding + self.queued + self.building + self.deferred
    }
}

/// Earliest in-progress project that will materially change the rail network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NextNetworkChange {
    pub(super) project_id: InfrastructureProjectId,
    pub(super) opens_at: UtcSeconds,
}

impl NextNetworkChange {
    fn from_state(state: &GameState) -> Option<Self> {
        state
            .region
            .rail_authority
            .infrastructure_projects
            .iter()
            .filter(|project| project.status == InfrastructureProjectStatus::Construction)
            .filter_map(|project| {
                project
                    .timeline
                    .planned_completion_at
                    .map(|opens_at| Self {
                        project_id: project.id,
                        opens_at,
                    })
            })
            .min_by_key(|change| change.opens_at)
    }
}

/// Stable presentation snapshot consumed by Authority dashboard renderers.
///
/// Keeping these derivations outside Ratatui rendering gives later redesign
/// batches one place to add stage-aware progress and programme ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AuthorityDashboardSnapshot {
    pub(super) treasury: Money,
    pub(super) available_investment: Option<Money>,
    pub(super) maintenance_reserve: Money,
    pub(super) committed_investment: Money,
    pub(super) public_allocation: Money,
    pub(super) access_fee_revenue: Money,
    pub(super) next_fiscal_period_at: Option<UtcSeconds>,
    pub(super) station_count: usize,
    pub(super) segment_count: usize,
    pub(super) construction_capacity: u32,
    pub(super) reserved_construction: u32,
    pub(super) active_construction: u32,
    pub(super) free_construction: u32,
    pub(super) programme: ProgrammeCounts,
    pub(super) next_network_change: Option<NextNetworkChange>,
}

impl AuthorityDashboardSnapshot {
    pub(super) fn from_state(state: &GameState) -> Self {
        let authority = &state.region.rail_authority;
        let finances = &authority.finances;
        Self {
            treasury: finances.treasury,
            available_investment: finances.uncommitted_investment().ok(),
            maintenance_reserve: finances.maintenance_reserve,
            committed_investment: finances.committed_investment,
            public_allocation: finances.regional_public_allocation,
            access_fee_revenue: finances.infrastructure_access_fee_revenue,
            next_fiscal_period_at: finances.next_fiscal_period_at,
            station_count: authority.rail_network.rail_stations.len(),
            segment_count: authority.rail_network.rail_lines.len(),
            construction_capacity: authority.construction_capacity,
            reserved_construction: authority.reserved_construction_count(),
            active_construction: authority.active_construction_count(),
            free_construction: authority.construction_slots_remaining(),
            programme: ProgrammeCounts::from_state(state),
            next_network_change: NextNetworkChange::from_state(state),
        }
    }
}
