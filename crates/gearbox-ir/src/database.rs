//! Database intent and resolved connection settings. No credential values.
use crate::GearId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

/// A file below the runtime's `home_dir/<gear-id>/` directory.
///
/// This selects SQLite independently of the profile's Postgres connection.
/// Relative files keep the product portable across operating systems.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SqliteDatabase {
    pub file: String,
}

impl SqliteDatabase {
    /// Refuse memory databases, absolute paths and escapes from the gear home.
    ///
    /// # Errors
    /// Returns an actionable reason when a file cannot represent durable storage.
    pub fn validate(&self) -> Result<(), String> {
        if self.file.is_empty()
            || self.file.contains(['\\', ':', '\0', '$', '?', '#'])
            || self
                .file
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("SQLite file must be a nonempty relative slash-separated path below the gear home, without traversal, a DSN or an environment reference".to_owned());
        }
        Ok(())
    }
}

/// One externally managed Postgres server for a deployment profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PostgresDatabase {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Only `${ENV_NAME}`; never an expanded credential or a default.
    pub password: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Overrides in intent; the complete DB-gear mapping in a resolved lock.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub database_names: BTreeMap<GearId, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_secret: Option<String>,
}

impl PostgresDatabase {
    /// The environment variable the runtime expands for the password.
    #[must_use]
    pub fn password_env(&self) -> Option<&str> {
        let name = self.password.strip_prefix("${")?.strip_suffix('}')?;
        let mut chars = name.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
            || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        Some(name)
    }

    /// Validate connection fields and operator-provided database names.
    ///
    /// # Errors
    /// Returns an actionable reason; passwords are never included in it.
    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() || self.user.trim().is_empty() || self.port == 0 {
            return Err("postgres requires a nonempty host/user and port 1..65535".to_owned());
        }
        if self.password_env().is_none() {
            return Err(
                "postgres password must be a ${ENV_NAME} reference, without a literal or default"
                    .to_owned(),
            );
        }
        if let Some(secret) = &self.existing_secret
            && (secret.len() > 253
                || secret.is_empty()
                || secret.split('.').any(|label| {
                    label.len() > 63
                        || label.is_empty()
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                }))
        {
            return Err("existing_secret must be a Kubernetes Secret DNS name".to_owned());
        }
        let mut names = BTreeSet::new();
        for name in self.database_names.values() {
            Self::validate_name(name)?;
            if !names.insert(name) {
                return Err(format!(
                    "database name `{name}` is assigned to more than one gear"
                ));
            }
        }
        Ok(())
    }

    /// Validate an unquoted PostgreSQL database identifier (maximum 63 ASCII bytes).
    ///
    /// # Errors
    /// Returns a reason when an override or a derived name cannot be used.
    pub fn validate_name(name: &str) -> Result<(), String> {
        let mut bytes = name.bytes();
        if name.len() > 63
            || !bytes
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
            || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(format!(
                "database name `{name}` must be 1..63 lowercase ASCII letters, digits or underscores, starting with a letter or underscore; set database_names for this gear"
            ));
        }
        Ok(())
    }
}
