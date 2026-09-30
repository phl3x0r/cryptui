//! CryptUI — a terminal UI for monitoring multiple crypto exchange accounts.
//!
//! The crate is deliberately split into a library plus a thin binary so every
//! layer below the terminal can be driven headlessly: the UI renders into a
//! ratatui `TestBackend` buffer in tests, and the venue clients are exercised
//! against recorded fixtures instead of the network.
//!
//! Phase 0 contains only the skeleton that the build and CI pipeline verify.

pub mod auth;
pub mod config;
pub mod venue;

/// Semantic version of the running binary, as reported by `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_reported() {
        assert!(!VERSION.is_empty(), "package version must not be empty");
    }
}
