use crate::ids::OrganizationId;
use chrono::{DateTime, Utc};

/// One tenant. The pilot runs single-tenant, but every owned record carries
/// `organization_id` from day one (architecture 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organization {
    pub id: OrganizationId,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}
