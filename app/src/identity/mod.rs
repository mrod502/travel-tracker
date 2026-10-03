//! Derived device identity: from stored advertisements to devices that persist.
//!
//! `bt_iden` answers "is this advertisement the same device as that one" and forgets the
//! answer. `occurrences` record advertisements forever and never ask the question. This
//! module is the missing layer between them, and it exists in this crate rather than in
//! either of those because it is the only thing that needs both: the resolver's reasoning
//! and the database's history.
//!
//! It closes two gaps in the gap analysis, and they are the same gap seen from two sides:
//!
//! * **`bt_iden` had no consumer, no tables, and no call site.** The resolvers here read
//!   `occurrences`, and the four derived tables — `device_identities`,
//!   `device_address_links`, `co_occurrence_events`, `association_edges` — are what a pass
//!   writes.
//! * **`bt_mon` could not feed `bt_iden`.** Nearly a fifth of the scoring model had no
//!   source at all and a sixth was scored from the wrong datum, which is only visible if
//!   the feed says where each value came from. [`adapt`] is where that is declared;
//!   [`bt_iden::evidence`] is what it is declared in.
//!
//! # Three kinds of thing, kept apart
//!
//! An **observation** ([`adapt`]) is one advertisement, as far as a feed can describe it.
//! An **identity** ([`replay`]) is what a resolver decided a set of observations had in
//! common, valid inside one process, with its evidence attached. A **device**
//! ([`fingerprint`]) is what those decisions agreed on often enough to write down, keyed
//! by the feature set rather than by any identifier.
//!
//! Collapsing the second into the third is the mistake this layout is built to avoid: a
//! resolver's id is a session, a fingerprint is a hypothesis about the world, and a device
//! that rotates its address legitimately produces several of the first and one of the
//! second.
//!
//! # Nothing here is live
//!
//! `FullNode` does not call into this module. The batch runs over stored rows, writes only
//! derived tables, and reports every decision it made, so that the merges can be argued
//! with before anything downstream depends on them.
//!
//! ```text
//! app identity-replay --last 7d              # read, print, write nothing
//! app identity-replay --last 7d --write      # and persist the four tables
//! app identity-replay --last 7d --json       # the same pass, machine-readable
//! ```

pub mod adapt;
pub mod fingerprint;
pub mod replay;
pub mod store;

// Only the surface the CLI drives is re-exported. A binary crate warns about a `pub use`
// nothing uses, and a re-export kept for a caller that does not exist yet is one more thing
// a reader has to work out the purpose of; the rest is reachable through its module.
pub use adapt::observation_from_occurrence;
pub use replay::{replay, ReplayOptions};
pub use store::{resolver_version, write_report};
