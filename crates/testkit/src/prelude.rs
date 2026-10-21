//! Commonly used testkit types and traits in one import:
//! `use soroban_testkit::prelude::*;`.
//!
//! # Prelude or module-specific imports?
//!
//! The prelude is a convenience, not the only supported entry point. It
//! re-exports the items most test files reach for every day:
//!
//! - [`TestEnv`] and [`TestkitError`] from `core`
//! - [`AuthMatrix`] from `auth`
//! - [`CapturedEvent`] and [`EventLog`] from `events`
//! - [`adversarial_amounts`], [`amounts_in`], [`bps_values`], and
//!   [`Conservation`] from `money`
//! - [`TestToken`] from `tokens`
//! - [`StorageKind`] from `ttl`
//!
//! Use the prelude when a test needs several of these items, or when a
//! single import keeps the test focused on behavior. Prefer a
//! module-specific import when:
//!
//! - you need only one or two items;
//! - the owning module provides useful context, as with
//!   `ledger::LEDGER_CLOSE_TIME_SECS`;
//! - the item is not re-exported through the prelude, such as
//!   `money::overflow_edge_triples`, `money::assert_conserved_over`, or
//!   `ledger::LEDGER_CLOSE_TIME_SECS`;
//! - explicit paths improve readability or avoid an ambiguous name.
//!
//! # Prelude import
//!
//! ```
//! use soroban_sdk::symbol_short;
//! use soroban_testkit::prelude::*;
//!
//! // Setup: an environment and a funded token.
//! let env = TestEnv::new();
//! let token = env.token();
//! let alice = env.address();
//! token.mint_raw(&alice, 1_000);
//!
//! // Operation: capture the event log and read the token balance.
//! let events: EventLog = env.events();
//! token.assert_balance(&alice, 1_000);
//!
//! // Assertions: prelude types are available without module paths.
//! events.assert_none(symbol_short!("transfer"));
//! Conservation {
//!     deposited: 1_000,
//!     withdrawn: 0,
//!     refunded: 0,
//!     remaining: 1_000,
//! }
//! .assert_holds();
//! ```
//!
//! # Module-specific imports
//!
//! ```
//! use soroban_testkit::core::TestEnv;
//! use soroban_testkit::ledger::LEDGER_CLOSE_TIME_SECS;
//! use soroban_testkit::money::{assert_conserved_over, Conservation};
//! use soroban_testkit::ttl::StorageKind;
//!
//! // Setup: a fresh environment and its starting ledger clock.
//! let env = TestEnv::new();
//! let start = env.now();
//! let start_sequence = env.sequence();
//!
//! // Operation: advance the ledger by one hour.
//! env.advance_hours(1);
//!
//! // Assertions: explicit paths keep the owning module visible.
//! assert_eq!(env.now(), start + 3_600);
//! assert_eq!(
//!     env.sequence() - start_sequence,
//!     (3_600 / LEDGER_CLOSE_TIME_SECS) as u32
//! );
//! assert_conserved_over(&env, 1, |_env, _step| Conservation {
//!     deposited: 10,
//!     withdrawn: 10,
//!     refunded: 0,
//!     remaining: 0,
//! });
//! let _kind = StorageKind::Instance;
//! ```

pub use crate::auth::AuthMatrix;
pub use crate::core::{TestEnv, TestkitError};
pub use crate::events::{CapturedEvent, EventLog};
pub use crate::money::{adversarial_amounts, amounts_in, bps_values, Conservation};
pub use crate::tokens::TestToken;
pub use crate::ttl::StorageKind;
