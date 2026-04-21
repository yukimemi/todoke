//! Backends for the `kind` field on a `[todoke.<name>]` target.
//!
//! Each backend exposes its own `dispatch` method because their natural call
//! shapes differ (neovim is async due to RPC, exec is sync). A unifying
//! trait was tried during scaffold but didn't carry its weight.

pub mod exec;
pub mod neovim;
