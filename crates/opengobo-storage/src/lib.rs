use opengobo_fixture::OflFixtureDefinition;
use opengobo_protocol::{
    EffectivePermissions, FixtureGroup, PatchedFixture, PermissionOverride, Role, Scene, UserId,
    UserProfile, UserView,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// SQLite DDL schema for a single-file `.gobo` show database.
pub const GOBO_SQLITE_SCHEMA: &str = r"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS show_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS server_identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    cert_pem TEXT NOT NULL,
    key_pem TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS roles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    permissions_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    preferences_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS user_roles (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);

CREATE TABLE IF NOT EXISTS user_permission_overrides (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    effect TEXT NOT NULL,
    resource TEXT NOT NULL,
    access_level TEXT NOT NULL,
    scope_type TEXT NOT NULL,
    scope_target_id TEXT
);

CREATE TABLE IF NOT EXISTS user_views (
    id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    layout_json TEXT NOT NULL,
    PRIMARY KEY (user_id, id)
);

CREATE TABLE IF NOT EXISTS ofl_fixtures (
    key TEXT PRIMARY KEY,
    definition_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS patched_fixtures (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    fixture_type_key TEXT NOT NULL REFERENCES ofl_fixtures(key) ON DELETE RESTRICT,
    mode_name TEXT NOT NULL,
    universe INTEGER NOT NULL,
    start_address INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS fixture_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    fixture_ids_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scenes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    fade_in_ms INTEGER NOT NULL DEFAULT 0,
    fade_out_ms INTEGER NOT NULL DEFAULT 0,
    values_json TEXT NOT NULL
);
";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error accessing showfile: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error in showfile: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("username '{0}' already exists")]
    DuplicateUsername(String),
    #[error("user '{0}' not found")]
    UserNotFound(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTlsIdentity {
    pub cert_pem: String,
    pub key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredUser {
    pub id: UserId,
    pub username: String,
    pub password_hash: String,
    pub role_ids: Vec<String>,
    pub overrides: Vec<PermissionOverride>,
    pub views: Vec<UserView>,
    pub preferences_json: String,
}

impl StoredUser {
    #[must_use]
    pub fn to_profile(&self) -> UserProfile {
        UserProfile {
            id: self.id.clone(),
            username: self.username.clone(),
            roles: self.role_ids.clone(),
            views: self.views.clone(),
            preferences_json: self.preferences_json.clone(),
        }
    }
}

/// Persistent state of a `.gobo` showfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowDocument {
    pub show_title: String,
    pub server_identity: Option<ServerTlsIdentity>,
    pub roles: BTreeMap<String, Role>,
    pub users: BTreeMap<UserId, StoredUser>,
    pub fixture_library: BTreeMap<String, OflFixtureDefinition>,
    pub patch: Vec<PatchedFixture>,
    pub groups: Vec<FixtureGroup>,
    pub scenes: Vec<Scene>,
}

impl Default for ShowDocument {
    fn default() -> Self {
        Self::new("Default Show")
    }
}

impl ShowDocument {
    #[must_use]
    pub fn new(show_title: impl Into<String>) -> Self {
        let roles = [Role::admin(), Role::operator(), Role::playback_only()]
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();

        let default_par = OflFixtureDefinition::generic_dimmer_rgb_par();
        let fixture_library = BTreeMap::from([(default_par.key.clone(), default_par)]);

        Self {
            show_title: show_title.into(),
            server_identity: None,
            roles,
            users: BTreeMap::new(),
            fixture_library,
            patch: Vec::new(),
            groups: Vec::new(),
            scenes: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_setup_mode_required(&self) -> bool {
        self.users.is_empty()
    }

    #[must_use]
    pub fn effective_permissions_for(&self, user_id: &str) -> Option<EffectivePermissions> {
        let user = self.users.get(user_id)?;
        let role_permissions = user
            .role_ids
            .iter()
            .filter_map(|id| self.roles.get(id))
            .flat_map(|r| r.permissions.iter().cloned())
            .collect();

        Some(EffectivePermissions {
            role_permissions,
            user_overrides: user.overrides.clone(),
        })
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if path.exists() {
            Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
        } else {
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Default Show");
            let doc = Self::new(title);
            doc.save(path)?;
            Ok(doc)
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opengobo_protocol::{AccessLevel, Resource};

    #[test]
    fn fresh_show_requires_setup_and_resolves_admin_permissions() {
        let mut doc = ShowDocument::new("Festival 2026");
        assert!(doc.is_setup_mode_required());

        doc.users.insert(
            "u-admin".to_string(),
            StoredUser {
                id: "u-admin".to_string(),
                username: "admin".to_string(),
                password_hash: "hashed".to_string(),
                role_ids: vec!["admin".to_string()],
                overrides: Vec::new(),
                views: Vec::new(),
                preferences_json: "{}".to_string(),
            },
        );
        assert!(!doc.is_setup_mode_required());

        let perms = doc
            .effective_permissions_for("u-admin")
            .unwrap_or_else(|| panic!("user should exist"));
        assert!(perms.is_allowed(Resource::System, AccessLevel::Write, None));
        assert!(perms.is_allowed(Resource::Patch, AccessLevel::Write, None));
    }
}
