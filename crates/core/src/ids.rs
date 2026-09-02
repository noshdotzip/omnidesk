//! Strongly-typed identifiers.
//!
//! These are newtypes rather than bare `String`/`u64` so that a `DeviceId` can never
//! be accidentally used where a `SessionId` is expected — a real source of bugs in
//! routing/loop-prevention code where several ids flow through the same functions.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        // Intentionally no `Default`: a "default id" that silently mints a random uuid
        // is a footgun in routing/loop code. Callers must be explicit via `new()`.
        #[allow(clippy::new_without_default)]
        impl $name {
            /// Generate a fresh random id. (Not available in no-entropy contexts, but
            /// this crate is only used inside the agent process.)
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

uuid_id!(
    /// Persistent identity of a paired device (derived from its Ed25519 public key at
    /// the identity layer; represented here as an opaque uuid for routing/tests).
    DeviceId
);
uuid_id!(
    /// One authenticated control session between two devices.
    SessionId
);
uuid_id!(
    /// One input lease — exclusive authorization to route a controller's input stream
    /// to a single active target at a time.
    LeaseId
);
uuid_id!(
    /// One projection (a source window projected to one destination).
    ProjectionId
);
uuid_id!(
    /// One input or clipboard event, used for de-duplication and loop prevention.
    EventId
);
