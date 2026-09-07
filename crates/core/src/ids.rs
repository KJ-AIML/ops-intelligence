use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Typed UUID newtypes (architecture 5). Stops an organization id being passed
/// where a source id is expected, which is the whole point of tenant scoping.
macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

typed_id!(OrganizationId);
typed_id!(SourceId);
typed_id!(RawSignalId);
typed_id!(EventId);
typed_id!(InsightId);
typed_id!(DatasetId);
typed_id!(RunId);
typed_id!(IncidentId);
