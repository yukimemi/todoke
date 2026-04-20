//! Editor backends. `kind` in config selects one of these.
//!
//! Each backend exposes its own `dispatch` method because their natural call
//! shapes differ (neovim is async due to RPC, generic is sync). A unifying
//! trait was tried during scaffold but didn't carry its weight.

pub mod generic;
pub mod neovim;
