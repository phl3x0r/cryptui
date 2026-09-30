//! Where an account's daily value history lives between runs.
//!
//! The venue serves at most a few months of income history, so a year-long view
//! can only exist if it is recorded as it happens. Each account gets a small
//! JSON file under the state directory, merged on read and rewritten on change;
//! a year of daily points is a few tens of kilobytes.

use std::path::{Path, PathBuf};

use crate::logging;
use crate::performance::{EquityPoint, EquitySeries};
use crate::venue::Venue;

/// The best history available: what the venue can reconstruct, with locally
/// recorded days filling in whatever it cannot see.
///
/// The venue's daily aggregates know about deposits and withdrawals, so they win
/// where the two overlap; the recordings are the only source for days the venue
/// will not serve, which is most of a year.
pub async fn load(store: Option<&Store>, venue: &dyn Venue, since_ms: i64) -> EquitySeries {
    let mut series = store.map(Store::load).unwrap_or_default();

    match venue.equity_history(since_ms).await {
        Ok(fetched) if !fetched.is_empty() => {
            series.merge_preferring(fetched);
            // A fixture's synthetic series needs no cache.
            if venue.records_history()
                && let Some(store) = store
                && let Err(error) = store.save(&series)
            {
                tracing::warn!(%error, "could not cache the history");
            }
            series
        }
        Ok(_) => series,
        Err(error) => {
            tracing::warn!(%error, "could not reconstruct history from the venue");
            series
        }
    }
}

/// What a store does when it can, and what it does when it cannot.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// The store for an account: `<state>/equity/<account>.json`.
    ///
    /// Returns `None` when no state directory can be determined, in which case
    /// the caller keeps the history in memory only.
    pub fn for_account(account: &str) -> Option<Self> {
        Self::for_account_from(account, &|name| std::env::var(name).ok())
    }

    /// [`Store::for_account`] with an explicit environment lookup.
    pub fn for_account_from(account: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let directory = logging::state_dir_from(env)?.join("equity");
        Some(Self {
            path: directory.join(format!("{}.json", sanitise(account))),
        })
    }

    /// A store at an explicit path, for tests and unusual setups.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the history is kept.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the stored series.
    ///
    /// A missing file is an empty history; a corrupt one is reported and treated
    /// as empty rather than taking the UI down.
    pub fn load(&self) -> EquitySeries {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return EquitySeries::default();
        };
        match serde_json::from_str::<Vec<EquityPoint>>(&raw) {
            Ok(points) => EquitySeries::new(points),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "ignoring unreadable history file");
                EquitySeries::default()
            }
        }
    }

    /// Write the series, creating the directory if needed.
    pub fn save(&self, series: &EquitySeries) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_string(series.points()).unwrap_or_else(|_| "[]".to_owned());
        std::fs::write(&self.path, encoded)
    }

    /// Merge points into the stored series and persist the result.
    ///
    /// `prefer_venue` decides the merge rule: the venue's daily aggregates know
    /// about deposits and withdrawals, so they win where the two overlap.
    pub fn record(
        &self,
        points: Vec<EquityPoint>,
        prefer_venue: bool,
    ) -> std::io::Result<EquitySeries> {
        let mut series = self.load();
        let incoming = EquitySeries::new(points);

        if prefer_venue {
            // The venue's day aggregates know about deposits and withdrawals.
            series.merge_preferring(incoming);
        } else {
            // A local observation is the fresher truth for its own day, and
            // `extend` keeps the later balance while accumulating its flows.
            series.extend(incoming.points().to_vec());
        }

        self.save(&series)?;
        Ok(series)
    }
}

/// Keep an account name usable as a file name.
fn sanitise(account: &str) -> String {
    account
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Store, sanitise};
    use crate::performance::{DAY_MS, EquityPoint, EquitySeries};

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cryptui-history-{label}-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn point(day: i64, wallet: f64, flow: f64) -> EquityPoint {
        EquityPoint {
            time_ms: day * DAY_MS,
            wallet,
            external_flow: flow,
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let store = Store::at(temp_path("missing"));
        assert!(store.load().is_empty());
    }

    #[test]
    fn recording_accumulates_and_survives_a_reload() {
        let path = temp_path("roundtrip");
        let store = Store::at(&path);

        store
            .record(vec![point(0, 100.0, 0.0)], false)
            .expect("first write");
        store
            .record(vec![point(1, 110.0, 0.0)], false)
            .expect("second write");

        let reloaded = store.load();
        assert_eq!(reloaded.len(), 2, "both days are kept");
        assert_eq!(reloaded.points()[1].wallet, 110.0);
        assert_eq!(
            Store::at(&path).load().len(),
            2,
            "the history is on disk, not just in memory"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_local_observation_refreshes_the_same_day() {
        let path = temp_path("upsert");
        let store = Store::at(&path);

        store
            .record(vec![point(5, 100.0, 0.0)], false)
            .expect("write");
        store
            .record(vec![point(5, 123.0, 0.0)], false)
            .expect("update");

        let series = store.load();
        assert_eq!(series.len(), 1, "one point per day");
        assert_eq!(series.points()[0].wallet, 123.0, "the latest balance wins");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_venue_backfill_supersedes_a_local_observation() {
        // The venue knows the day included a deposit; the recorder does not.
        let path = temp_path("venue-wins");
        let store = Store::at(&path);

        store
            .record(vec![point(3, 200.0, 0.0)], false)
            .expect("local");
        let series = store
            .record(vec![point(3, 200.0, 100.0)], true)
            .expect("venue");

        assert_eq!(series.points()[0].external_flow, 100.0);
        assert_eq!(
            series.metrics().map(|m| m.total_return),
            None,
            "one point is not measurable, but the flow is recorded"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unreadable_file_is_treated_as_empty() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ not json").expect("write junk");
        let store = Store::at(&path);

        assert!(store.load().is_empty(), "corruption does not panic");
        store
            .record(vec![point(0, 100.0, 0.0)], false)
            .expect("recovers by rewriting");
        assert_eq!(store.load().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_store_path_is_derived_from_the_state_directory() {
        let env = |name: &str| match name {
            "XDG_STATE_HOME" => Some("/state".to_owned()),
            _ => None,
        };

        let store = Store::for_account_from("main", &env).expect("a path");
        assert_eq!(
            store.path(),
            std::path::Path::new("/state/cryptui/equity/main.json")
        );

        let odd = Store::for_account_from("sub/account name", &env).expect("a path");
        assert_eq!(
            odd.path().file_name().and_then(|name| name.to_str()),
            Some("sub_account_name.json"),
            "an account name cannot escape the directory"
        );

        assert!(Store::for_account_from("main", &|_| None).is_none());
    }

    #[test]
    fn account_names_are_made_safe() {
        assert_eq!(sanitise("main"), "main");
        assert_eq!(sanitise("paper-2_test"), "paper-2_test");
        assert_eq!(
            sanitise("../../etc/passwd"),
            "______etc_passwd",
            "dots go too, so a name cannot climb out of the directory"
        );
        assert_eq!(sanitise("a b"), "a_b");
    }

    #[test]
    fn a_written_series_reloads_to_the_same_metrics() {
        let path = temp_path("metrics");
        let store = Store::at(&path);
        let series = EquitySeries::new(vec![
            point(0, 100.0, 0.0),
            point(1, 120.0, 0.0),
            point(2, 90.0, 0.0),
        ]);

        store.save(&series).expect("save");
        let reloaded = store.load();

        assert_eq!(reloaded.points(), series.points());
        assert_eq!(
            format!("{:?}", reloaded.metrics()),
            format!("{:?}", series.metrics()),
            "the figures are identical after a round trip"
        );

        let _ = std::fs::remove_file(&path);
    }
}
