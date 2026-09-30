//! Configuration loading.
//!
//! The configuration file is resolved from the first available source:
//!
//! 1. `--config <PATH>`
//! 2. `$CRYPTUI_CONFIG`
//! 3. `~/.config/cryptui/config.toml`
//!
//! String values may reference environment variables as `${NAME}`. References
//! are resolved *after* parsing, so comments and unrelated text are never
//! touched, and a `${...}` sequence that is not a valid variable name is kept
//! verbatim — which keeps pasted secrets containing `$` intact.
//!
//! Credentials are held in [`Secret`], whose `Debug` output is redacted, so a
//! key can never reach a log line or a panic message by accident.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::venue::{Interval, VenueId};

/// Environment variable that overrides the configuration path.
pub const CONFIG_PATH_ENV: &str = "CRYPTUI_CONFIG";

/// Configuration path relative to the user's home directory.
pub const DEFAULT_CONFIG_PATH: &str = ".config/cryptui/config.toml";

/// Lowest accepted `settings.refresh_interval_ms`, to keep the request rate sane.
const MIN_REFRESH_INTERVAL_MS: u64 = 250;

/// Everything that can go wrong while resolving, reading or validating the
/// configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No path could be determined (no override and no `$HOME`).
    #[error(
        "no configuration path: pass --config <PATH> or set ${CONFIG_PATH_ENV}, \
         or create ~/{DEFAULT_CONFIG_PATH}"
    )]
    NoPath,

    /// The file could not be read.
    #[error("cannot read configuration file {path}: {source}")]
    Read {
        /// Path that was attempted.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file is not valid TOML, or does not match the expected schema.
    #[error("invalid configuration file {path}: {source}")]
    Parse {
        /// Path that was parsed.
        path: PathBuf,
        /// Underlying TOML error, including line and column.
        #[source]
        source: toml::de::Error,
    },

    /// A `${NAME}` reference has no corresponding environment variable.
    #[error("environment variable {name} referenced by the configuration is not set")]
    MissingEnv {
        /// Name of the unset variable.
        name: String,
    },

    /// The file parsed but describes an unusable setup.
    #[error("configuration is invalid: {0}")]
    Invalid(String),
}

/// A credential string that never appears in `Debug`, `Display` or logs.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Borrow the underlying value. Call sites should hand it straight to the
    /// signing code and never format it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the value is empty or only whitespace, which means "not filled in".
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
impl Secret {
    /// Build a secret from a literal, for tests that need a known value.
    pub(crate) fn from_test_value(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// How an account obtains its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountMode {
    /// Talk to the live venue, optionally against its testnet.
    Api {
        /// Whether the venue's testnet endpoints should be used.
        testnet: bool,
    },
    /// Serve recorded payloads instead of calling the network.
    Fixture {
        /// Fixture location as written in the configuration.
        path: PathBuf,
    },
}

/// A single configured account.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    venue: VenueId,
    label: String,
    #[serde(default)]
    api_key: Option<Secret>,
    #[serde(default)]
    api_secret: Option<Secret>,
    #[serde(default)]
    testnet: bool,
    #[serde(default)]
    fixture: Option<PathBuf>,
}

impl Account {
    /// Venue this account connects to.
    pub fn venue(&self) -> VenueId {
        self.venue
    }

    /// Human-readable name shown in the header and account switcher.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Credentials for the live venue.
    pub fn api_key(&self) -> Option<&Secret> {
        self.api_key.as_ref()
    }

    /// Secret for the live venue.
    pub fn api_secret(&self) -> Option<&Secret> {
        self.api_secret.as_ref()
    }

    /// Resolve how this account is fed, rejecting half-configured accounts.
    pub fn mode(&self, name: &str) -> Result<AccountMode, ConfigError> {
        if self.fixture.is_some() {
            if self.api_key.is_some() || self.api_secret.is_some() {
                return Err(ConfigError::Invalid(format!(
                    "account `{name}` sets both `fixture` and API credentials; use one or the other"
                )));
            }
            return Ok(AccountMode::Fixture {
                path: self.fixture.clone().unwrap_or_default(),
            });
        }

        let key = self.api_key.as_ref().filter(|key| !key.is_blank());
        let secret = self.api_secret.as_ref().filter(|secret| !secret.is_blank());
        match (key, secret) {
            (Some(_), Some(_)) => Ok(AccountMode::Api {
                testnet: self.testnet,
            }),
            _ => Err(ConfigError::Invalid(format!(
                "account `{name}` is incomplete: fill in both `api_key` and `api_secret`, \
                 or point `fixture` at a recorded payload"
            ))),
        }
    }

    /// One-line description safe to print: never includes credential material.
    pub fn describe_mode(&self, name: &str) -> String {
        match self.mode(name) {
            Ok(AccountMode::Api { testnet: false }) => "live API credentials".to_owned(),
            Ok(AccountMode::Api { testnet: true }) => "testnet API credentials".to_owned(),
            Ok(AccountMode::Fixture { path }) => format!("fixture {}", path.display()),
            Err(error) => format!("unusable ({error})"),
        }
    }
}

/// Global, account-independent settings.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "default_refresh_interval_ms")]
    refresh_interval_ms: u64,
    #[serde(default = "default_interval")]
    default_interval: Interval,
    #[serde(default = "default_history_candles")]
    chart_history_candles: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_interval_ms: default_refresh_interval_ms(),
            default_interval: default_interval(),
            chart_history_candles: default_history_candles(),
        }
    }
}

impl Settings {
    /// How often signed REST data (positions and balances) is refreshed.
    pub fn refresh_interval_ms(&self) -> u64 {
        self.refresh_interval_ms
    }

    /// Candle interval the chart opens with.
    pub fn default_interval(&self) -> Interval {
        self.default_interval
    }

    /// How many historical candles to fetch per chart load.
    pub fn chart_history_candles(&self) -> u32 {
        self.chart_history_candles
    }
}

fn default_refresh_interval_ms() -> u64 {
    3000
}

fn default_interval() -> Interval {
    Interval::M15
}

fn default_history_candles() -> u32 {
    500
}

/// The parsed and validated configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    default_account: Option<String>,
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    accounts: BTreeMap<String, Account>,
    #[serde(skip)]
    literal_credentials: bool,
}

impl Config {
    /// Load the configuration from `path`, resolving `${NAME}` references from
    /// the process environment.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::load_with(path, &|name| std::env::var(name).ok())
    }

    /// Load the configuration from `path` using a custom variable lookup.
    ///
    /// Used by tests and by the headless subcommands so environment lookups are
    /// never implicit.
    pub fn load_with(
        path: &Path,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut document: toml::Value =
            toml::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        // Must be decided before interpolation, otherwise every reference has
        // already been replaced by its value and looks literal.
        let literal_credentials = holds_literal_credentials(&document);
        interpolate_value(&mut document, lookup)?;
        let mut config: Self = document.try_into().map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.literal_credentials = literal_credentials;
        config.validate()?;
        Ok(config)
    }

    /// Whether the file itself contains credential material, rather than
    /// `${NAME}` references to the environment.
    ///
    /// Callers use this to decide whether insecure file permissions are worth
    /// warning about: a template full of `${VAR}` references is harmless.
    pub fn holds_literal_credentials(&self) -> bool {
        self.literal_credentials
    }

    /// Name of the account to activate at startup.
    ///
    /// Always present on a validated configuration.
    pub fn default_account(&self) -> &str {
        self.default_account.as_deref().unwrap_or_default()
    }

    /// Global settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Configured accounts, ordered by name.
    pub fn accounts(&self) -> &BTreeMap<String, Account> {
        &self.accounts
    }

    /// Look up a single account by name.
    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.get(name)
    }

    /// Check invariants that serde cannot express, and resolve
    /// `default_account` when it was omitted.
    fn validate(&mut self) -> Result<(), ConfigError> {
        if self.accounts.is_empty() {
            return Err(ConfigError::Invalid(
                "no accounts configured; add at least one [accounts.<name>] section".to_owned(),
            ));
        }

        for (name, account) in &self.accounts {
            account.mode(name)?;
            if account.label.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "account `{name}` has an empty `label`"
                )));
            }
        }

        match self.default_account.as_deref() {
            Some(name) if self.accounts.contains_key(name) => {}
            Some(name) => {
                return Err(ConfigError::Invalid(format!(
                    "`default_account = \"{name}\"` does not match any configured account"
                )));
            }
            None => {
                let first = self.accounts.keys().next().cloned().unwrap_or_default();
                tracing::debug!(account = %first, "no default_account configured, using first account");
                self.default_account = Some(first);
            }
        }

        if self.settings.refresh_interval_ms < MIN_REFRESH_INTERVAL_MS {
            return Err(ConfigError::Invalid(format!(
                "`settings.refresh_interval_ms` is {} but must be at least {MIN_REFRESH_INTERVAL_MS}",
                self.settings.refresh_interval_ms
            )));
        }

        Ok(())
    }
}

/// Resolve the configuration path from the first available source.
pub fn discover(cli_override: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(path) = cli_override {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(CONFIG_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or(ConfigError::NoPath)?;
    Ok(Path::new(&home).join(DEFAULT_CONFIG_PATH))
}

/// Warn when a configuration file holding credentials is readable by other
/// users.
///
/// Returns `None` on non-Unix platforms or when the mode is already safe.
#[cfg(unix)]
pub fn permissions_warning(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    if mode & 0o077 == 0 {
        return None;
    }
    Some(format!(
        "{} is readable by other users (mode {:04o}); run `chmod 600 {}`",
        path.display(),
        mode & 0o777,
        path.display()
    ))
}

/// See [`permissions_warning`]; file modes are a Unix concept.
#[cfg(not(unix))]
pub fn permissions_warning(_path: &Path) -> Option<String> {
    None
}

/// Replace `${NAME}` references inside every string of a parsed document.
fn interpolate_value(
    value: &mut toml::Value,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(text) => {
            *text = interpolate_string(text, lookup)?;
        }
        toml::Value::Array(items) => {
            for item in items {
                interpolate_value(item, lookup)?;
            }
        }
        toml::Value::Table(table) => {
            for item in table.iter_mut().map(|(_key, value)| value) {
                interpolate_value(item, lookup)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Replace `${NAME}` references in one string.
///
/// Sequences that do not name a valid variable (`${`, `${1x}`, `${unclosed`)
/// are left exactly as they are.
fn interpolate_string(
    input: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, ConfigError> {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(relative) = input[cursor..].find("${") {
        let start = cursor + relative;
        let name_start = start + 2;
        let Some(close) = input[name_start..].find('}') else {
            break;
        };
        let name = &input[name_start..name_start + close];
        if !is_env_name(name) {
            output.push_str(&input[cursor..name_start]);
            cursor = name_start;
            continue;
        }
        let value = lookup(name).ok_or_else(|| ConfigError::MissingEnv {
            name: name.to_owned(),
        })?;
        output.push_str(&input[cursor..start]);
        output.push_str(&value);
        cursor = name_start + close + 1;
    }

    output.push_str(&input[cursor..]);
    Ok(output)
}

/// Whether `name` is a usable environment variable name.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Whether any account in a *pre-interpolation* document carries a credential
/// value that is written out in full rather than referenced as `${NAME}`.
fn holds_literal_credentials(document: &toml::Value) -> bool {
    const CREDENTIAL_FIELDS: [&str; 2] = ["api_key", "api_secret"];

    let Some(accounts) = document.get("accounts").and_then(toml::Value::as_table) else {
        return false;
    };
    accounts
        .values()
        .filter_map(toml::Value::as_table)
        .any(|account| {
            CREDENTIAL_FIELDS.iter().any(|field| {
                account
                    .get(*field)
                    .and_then(toml::Value::as_str)
                    .is_some_and(|value| {
                        let trimmed = value.trim();
                        !trimmed.is_empty() && !trimmed.contains("${")
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Config, ConfigError, interpolate_string};

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn interpolates_references() {
        let result = interpolate_string("key=${KEY} tail", &lookup(&[("KEY", "abc")]));
        assert_eq!(result.unwrap(), "key=abc tail");
    }

    #[test]
    fn reports_missing_variables() {
        let error = interpolate_string("${NOPE}", &lookup(&[])).unwrap_err();
        assert!(matches!(error, ConfigError::MissingEnv { name } if name == "NOPE"));
    }

    #[test]
    fn leaves_non_references_untouched() {
        let cases = [
            "price$",
            "cost ${",
            "${1BAD}",
            "${UNCLOSED",
            "${kebab-case}",
            "literal ${{brace}}",
        ];
        for case in cases {
            assert_eq!(
                interpolate_string(case, &lookup(&[])).unwrap(),
                case,
                "`{case}` must survive interpolation verbatim"
            );
        }
    }

    #[test]
    fn interpolates_secrets_from_environment() {
        let result = interpolate_string(
            "${KEY}:${SECRET}",
            &lookup(&[("KEY", "k"), ("SECRET", "s")]),
        );
        assert_eq!(result.unwrap(), "k:s");
    }

    /// The committed template must stay loadable, or the README instructions
    /// would hand new users a broken starting point.
    #[test]
    fn committed_template_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.template.toml");
        let config = Config::load_with(
            &path,
            &lookup(&[
                ("BINANCE_API_KEY", "template-key"),
                ("BINANCE_API_SECRET", "template-secret"),
            ]),
        )
        .expect("template must load");

        assert_eq!(config.default_account(), "main");
        assert_eq!(config.accounts().len(), 2);
        assert_eq!(config.settings().refresh_interval_ms(), 3000);
        assert_eq!(config.settings().chart_history_candles(), 500);
        assert!(config.account("paper").is_some(), "fixture account present");
    }

    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.template.toml");
        let config = Config::load_with(
            &path,
            &lookup(&[
                ("BINANCE_API_KEY", "super-secret-key"),
                ("BINANCE_API_SECRET", "super-secret-value"),
            ]),
        )
        .expect("template must load");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"), "credentials stay redacted");
        assert!(
            !rendered.contains("super-secret"),
            "no credential material leaks"
        );
    }

    #[test]
    fn rejects_unknown_account_field() {
        let path = write_temp(
            "unknown-field",
            "[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\ntypo_field = 1\nfixture = \"f.json\"\n",
        );
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(error.to_string().contains("typo_field"), "got: {error}");
    }

    #[test]
    fn rejects_half_configured_account() {
        let path = write_temp(
            "half-configured",
            "[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\napi_key = \"only-key\"\n",
        );
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(error.to_string().contains("api_secret"), "got: {error}");
    }

    #[test]
    fn rejects_unknown_default_account() {
        let path = write_temp(
            "unknown-default",
            "default_account = \"ghost\"\n[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\nfixture = \"f.json\"\n",
        );
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(error.to_string().contains("ghost"), "got: {error}");
    }

    #[test]
    fn rejects_reckless_refresh_interval() {
        let path = write_temp(
            "fast-refresh",
            "[settings]\nrefresh_interval_ms = 10\n[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\nfixture = \"f.json\"\n",
        );
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(error.to_string().contains("250"), "got: {error}");
    }

    #[test]
    fn rejects_unsupported_interval() {
        let path = write_temp(
            "bad-interval",
            "[settings]\ndefault_interval = \"7m\"\n[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\nfixture = \"f.json\"\n",
        );
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(error.to_string().contains("7m"), "got: {error}");
    }

    #[test]
    fn missing_file_is_reported_with_its_path() {
        let path = PathBuf::from("/nonexistent/cryptui/config.toml");
        let error = Config::load_with(&path, &lookup(&[])).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("/nonexistent/cryptui/config.toml")
        );
    }

    #[test]
    fn recognises_reference_only_configuration() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.template.toml");
        let config = Config::load_with(
            &path,
            &lookup(&[
                ("BINANCE_API_KEY", "template-key"),
                ("BINANCE_API_SECRET", "template-secret"),
            ]),
        )
        .expect("template must load");

        assert!(
            !config.holds_literal_credentials(),
            "the template only references the environment"
        );
    }

    #[test]
    fn recognises_literal_credentials() {
        let path = write_temp(
            "literal-creds",
            "[accounts.main]\nvenue = \"binance_futures\"\nlabel = \"Main\"\napi_key = \"written-out-key\"\napi_secret = \"written-out-secret\"\n",
        );
        let config = Config::load_with(&path, &lookup(&[])).expect("config must load");

        assert!(
            config.holds_literal_credentials(),
            "a pasted credential must be reported so permissions can be flagged"
        );
    }

    /// Write a throwaway configuration file for the duration of one test.
    fn write_temp(label: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cryptui-config-{label}-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("temp config is writable");
        path
    }
}
