//! Where a sign-in's tokens are kept: a file of the person's own, outside every workspace.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{PLAN_SCOPE, random};

/// How long before a token runs out it is renewed, in seconds.
const MARGIN: u64 = 300;

/// One sign-in: who signed in, the client registered for them, and their tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The person's email, as the ID token gave it.
    #[serde(default)]
    pub email: Option<String>,
    /// Who issued the tokens.
    pub issuer: String,
    /// The account: the ID token's `sub`.
    pub subject: String,
    /// The client `OpenAI` issued for this person and workspace.
    pub client_id: String,
    /// The host the tokens were issued to.
    pub ext_agent_host_id: String,
    /// The ID token of the last sign-in: a hint at the next, never a credential.
    pub id_token: String,
    /// What requests are made with, good for `expires_in` seconds from `saved_at`.
    pub access_token: String,
    /// What renews the access token. Replaced at each renewal.
    pub refresh_token: String,
    /// `Bearer`.
    pub token_type: String,
    /// How long the access token is good for, in seconds.
    pub expires_in: u64,
    /// What was granted.
    pub scopes: Vec<String>,
    /// When the tokens were had, in seconds since the Unix epoch.
    pub saved_at: u64,
    /// When the service allows the next renewal, as it said it.
    #[serde(default)]
    pub earliest_refresh_at: Option<Value>,
}

impl fmt::Debug for Record {
    // The tokens are left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("email", &self.email)
            .field("client_id", &self.client_id)
            .field("scopes", &self.scopes)
            .field("saved_at", &self.saved_at)
            .finish_non_exhaustive()
    }
}

impl Record {
    /// When the access token runs out, in seconds since the Unix epoch.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.saved_at.saturating_add(self.expires_in)
    }

    /// Whether the access token is good at `now`, with time to spare.
    #[must_use]
    pub const fn fresh(&self, now: u64) -> bool {
        now.saturating_add(MARGIN) < self.expires_at()
    }

    /// Whether the access token is still good at `now`, if only just.
    #[must_use]
    pub const fn usable(&self, now: u64) -> bool {
        now < self.expires_at()
    }

    /// Whether the service allows a renewal at `now`. It does unless it named a later time,
    /// as seconds (or milliseconds) since the Unix epoch.
    #[must_use]
    pub fn renewable(&self, now: u64) -> bool {
        let earliest = self.earliest_refresh_at.as_ref().and_then(Value::as_u64);
        earliest.is_none_or(|at| {
            // A value this large is in milliseconds.
            let seconds = if at > 100_000_000_000 { at / 1000 } else { at };
            seconds <= now
        })
    }

    /// Whether the person let this tool use their plan.
    #[must_use]
    pub fn plan(&self) -> bool {
        self.scopes.iter().any(|scope| scope == PLAN_SCOPE)
    }
}

/// The file one sign-in is kept in, and the host's ID beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    file: PathBuf,
    host: PathBuf,
}

impl Store {
    /// A store in `file`, with the host's ID in `host`.
    #[must_use]
    pub fn at(file: impl Into<PathBuf>, host: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            host: host.into(),
        }
    }

    /// The store of the sign-in called `name` under `dir`, the person's own directory for
    /// this tool: `<dir>/chatgpt/<name>.json`, with the host's ID in `<dir>/host-id`.
    #[must_use]
    pub fn named(dir: &Path, name: &str) -> Self {
        Self::at(
            dir.join("chatgpt").join(format!("{name}.json")),
            dir.join("host-id"),
        )
    }

    /// The file the tokens are in.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    fn failed(&self, what: &str, error: &dyn fmt::Display) -> String {
        format!("{what} {}: {error}", self.file.display())
    }

    /// The sign-in kept here, if there is one.
    ///
    /// # Errors
    ///
    /// If the file is there and cannot be read, or is not a sign-in.
    pub fn load(&self) -> Result<Option<Record>, String> {
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.failed("cannot read", &error)),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| self.failed("no sign-in is in", &error))
    }

    /// Keep `record`, in place of what was here. The file is written whole beside the old
    /// one and moved over it, and is the person's alone to read.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn save(&self, record: &Record) -> Result<(), String> {
        let text = serde_json::to_string_pretty(record)
            .map_err(|error| self.failed("cannot write", &error))?;
        write_private(&self.file, &text).map_err(|error| self.failed("cannot write", &error))
    }

    /// Forget the sign-in. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// If the file is there and cannot be removed.
    pub fn remove(&self) -> Result<bool, String> {
        match fs::remove_file(&self.file) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(self.failed("cannot remove", &error)),
        }
    }

    /// This host's ID: made once, kept, and the same at every sign-in from here. It names
    /// the host to the service and is no credential.
    ///
    /// # Errors
    ///
    /// If it cannot be read, made or kept.
    pub fn host_id(&self) -> Result<String, String> {
        let failed = |error: &dyn fmt::Display| format!("{}: {error}", self.host.display());
        match fs::read_to_string(&self.host) {
            Ok(id) if !id.trim().is_empty() => return Ok(id.trim().to_owned()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(&error)),
        }
        let id = random::host_id()?;
        write_private(&self.host, &format!("{id}\n")).map_err(|error| failed(&error))?;
        Ok(id)
    }

    /// Take this store for one process at a time, until what is returned is dropped: a
    /// renewal replaces the refresh token, and two at once would lose one.
    ///
    /// # Errors
    ///
    /// If the lock file cannot be made or locked.
    pub fn lock(&self) -> Result<File, String> {
        let path = self.file.with_extension("lock");
        let failed = |error: std::io::Error| format!("cannot lock {}: {error}", path.display());
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(failed)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(failed)?;
        file.lock().map_err(failed)?;
        Ok(file)
    }
}

/// Write `text` to `path` for its owner alone, whole or not at all.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let scratch = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&scratch)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&scratch, path)
}

#[cfg(test)]
mod tests {
    use corduroy_test_support::scratch;
    use serde_json::json;

    use super::*;

    fn record(saved_at: u64) -> Record {
        Record {
            email: Some("person@example.com".to_owned()),
            issuer: "https://auth.openai.com".to_owned(),
            subject: "user-1".to_owned(),
            client_id: "oaiapp_1".to_owned(),
            ext_agent_host_id: "urn:uuid:0".to_owned(),
            id_token: "id".to_owned(),
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_in: 3600,
            scopes: vec!["openid".to_owned(), PLAN_SCOPE.to_owned()],
            saved_at,
            earliest_refresh_at: None,
        }
    }

    #[test]
    fn a_sign_in_is_kept_for_its_owner_alone_and_read_back() {
        let dir = scratch("chatgpt-store");
        let store = Store::named(&dir, "plan");
        assert_eq!(store.load().unwrap(), None);
        store.save(&record(100)).unwrap();
        assert_eq!(store.load().unwrap(), Some(record(100)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(store.file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store.remove().unwrap());
        assert!(!store.remove().unwrap());
    }

    #[test]
    fn the_host_keeps_its_id() {
        let dir = scratch("chatgpt-host");
        let store = Store::named(&dir, "plan");
        let id = store.host_id().unwrap();
        assert!(id.starts_with("urn:uuid:"), "{id}");
        assert_eq!(Store::named(&dir, "other").host_id().unwrap(), id);
    }

    #[test]
    fn a_token_is_renewed_before_it_runs_out_and_not_before_the_service_allows() {
        let kept = record(1000);
        assert!(kept.fresh(1000) && kept.fresh(4299));
        assert!(!kept.fresh(4300) && kept.usable(4599) && !kept.usable(4600));
        assert!(kept.renewable(0));
        let later = Record {
            earliest_refresh_at: Some(json!(2000)),
            ..record(1000)
        };
        assert!(!later.renewable(1999) && later.renewable(2000));
        let millis = Record {
            earliest_refresh_at: Some(json!(2_000_000_000_000_u64)),
            ..record(1000)
        };
        assert!(!millis.renewable(1_999_999_999) && millis.renewable(2_000_000_000));
        assert!(kept.plan());
        assert!(!format!("{kept:?}").contains("access"));
    }
}
