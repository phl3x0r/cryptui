//! Turning configuration entries into venue clients.
//!
//! The event loop needs to build a client for any configured account when the
//! user switches, so construction lives here rather than in the binary.

use std::path::PathBuf;

use crate::config::{Account, AccountMode, Config, ConfigError, Secret};
use crate::venue::binance_futures::BinanceFutures;
use crate::venue::fixture::FixtureVenue;
use crate::venue::{Venue, VenueError, VenueId};

/// A configured account, ready to be connected.
#[derive(Debug, Clone)]
pub struct AccountHandle {
    name: String,
    label: String,
    kind: Kind,
}

/// How an account is fed.
#[derive(Debug, Clone)]
enum Kind {
    /// Live venue credentials.
    Live {
        venue: VenueId,
        api_key: Secret,
        api_secret: Secret,
        testnet: bool,
    },
    /// Recorded payloads on disk.
    Fixture { path: PathBuf },
}

impl AccountHandle {
    /// Build a handle from one configuration entry.
    ///
    /// Credentials are validated here, so connecting later cannot fail because
    /// an account was half configured.
    pub fn from_config(name: &str, account: &Account) -> Result<Self, ConfigError> {
        let label = account.label().to_owned();

        let kind = match account.mode(name)? {
            AccountMode::Api { testnet } => {
                let missing = |field: &str| {
                    ConfigError::Invalid(format!("account `{name}` has no `{field}`"))
                };
                Kind::Live {
                    venue: account.venue(),
                    api_key: account
                        .api_key()
                        .cloned()
                        .ok_or_else(|| missing("api_key"))?,
                    api_secret: account
                        .api_secret()
                        .cloned()
                        .ok_or_else(|| missing("api_secret"))?,
                    testnet,
                }
            }
            AccountMode::Fixture { path } => Kind::Fixture { path },
        };

        Ok(Self {
            name: name.to_owned(),
            label,
            kind,
        })
    }

    /// Configuration key of this account.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human-readable label shown in the header.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Venue this account talks to.
    pub fn venue(&self) -> VenueId {
        match &self.kind {
            Kind::Live { venue, .. } => *venue,
            Kind::Fixture { .. } => VenueId::BinanceFutures,
        }
    }

    /// Whether this account reads from a fixture rather than the network.
    pub fn is_fixture(&self) -> bool {
        matches!(self.kind, Kind::Fixture { .. })
    }

    /// Build the venue client.
    pub fn connect(&self) -> Result<Box<dyn Venue>, VenueError> {
        match &self.kind {
            Kind::Live {
                venue,
                api_key,
                api_secret,
                testnet,
            } => match venue {
                VenueId::BinanceFutures => Ok(Box::new(BinanceFutures::new(
                    api_key.clone(),
                    api_secret.clone(),
                    *testnet,
                )?)),
            },
            Kind::Fixture { path } => Ok(Box::new(FixtureVenue::load(path)?)),
        }
    }
}

/// Every configured account, in configuration order.
pub fn handles(config: &Config) -> Result<Vec<AccountHandle>, ConfigError> {
    config
        .accounts()
        .iter()
        .map(|(name, account)| AccountHandle::from_config(name, account))
        .collect()
}

/// Index of the account to open, defaulting to the first.
pub fn default_index(config: &Config, handles: &[AccountHandle]) -> usize {
    handles
        .iter()
        .position(|handle| handle.name() == config.default_account())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::config::Config;

    use super::{default_index, handles};

    fn template_config() -> Config {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.template.toml");
        Config::load_with(&path, &|name| Some(format!("value-for-{name}"))).expect("template loads")
    }

    #[test]
    fn every_configured_account_becomes_a_handle() {
        let config = template_config();
        let handles = handles(&config).expect("handles build");

        let names: Vec<&str> = handles.iter().map(|handle| handle.name()).collect();
        assert_eq!(names, ["main", "paper"], "configuration order is preserved");
        assert_eq!(handles[0].label(), "Main");
        assert!(!handles[0].is_fixture());
        assert!(handles[1].is_fixture(), "the paper account is a fixture");
        assert_eq!(handles[1].venue(), crate::venue::VenueId::BinanceFutures);
    }

    #[test]
    fn the_configured_default_selects_the_opening_account() {
        let config = template_config();
        let handles = handles(&config).expect("handles build");
        assert_eq!(default_index(&config, &handles), 0, "`main` is the default");

        let mut handles = handles;
        handles.swap(0, 1);
        assert_eq!(
            default_index(&config, &handles),
            1,
            "found by name, not position"
        );
    }

    #[test]
    fn a_live_handle_connects_without_touching_the_network() {
        let config = template_config();
        let handles = handles(&config).expect("handles build");

        let venue = handles[0].connect().expect("a client is built");
        assert_eq!(venue.id(), crate::venue::VenueId::BinanceFutures);
    }

    #[tokio::test]
    async fn the_configured_fixture_connects_and_serves_data() {
        // The template points at the committed fixture, which is resolved
        // relative to the working directory the binary runs from.
        let config = template_config();
        let handles = handles(&config).expect("handles build");

        let venue = handles[1].connect().expect("the committed fixture loads");
        let positions = venue.positions().await.expect("positions");
        assert!(positions.len() >= 3, "fixture positions are available");
        assert!(
            venue.account().await.is_ok(),
            "fixture account is available"
        );
    }

    #[test]
    fn a_missing_fixture_fails_at_connect_time_with_its_path() {
        let path =
            std::env::temp_dir().join(format!("cryptui-accounts-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[accounts.paper]\nvenue = \"binance_futures\"\nlabel = \"Paper\"\nfixture = \"/nonexistent/paper.json\"\n",
        )
        .expect("temp config is writable");
        let config = Config::load(&path).expect("config loads");
        let handles = handles(&config).expect("handles build");

        // `expect_err` needs the success type to be `Debug`, and a boxed
        // `dyn Venue` deliberately is not.
        let error = match handles[0].connect() {
            Ok(_) => panic!("the fixture file does not exist"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("/nonexistent/paper.json"),
            "got: {error}"
        );
    }
}
