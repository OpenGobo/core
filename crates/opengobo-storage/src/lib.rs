use opengobo_fixture::OflFixtureDefinition;
use opengobo_protocol::{
    DmxAddress, EffectivePermissions, FixtureGroup, PatchedFixture, PermissionOverride, Role,
    Scene, UserId, UserProfile, UserView,
};
use rusqlite::{Connection, OptionalExtension, params};
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
    key_pem TEXT NOT NULL
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
    override_json TEXT NOT NULL
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
    #[error("SQLite database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error in showfile: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid DMX start address {0} in showfile")]
    InvalidDmxAddress(u16),
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

/// Persistent state of a `.gobo` SQLite showfile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            Self::load_from_sqlite(path)
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

        let mut conn = Connection::open(path)?;
        conn.execute_batch(GOBO_SQLITE_SCHEMA)?;

        let tx = conn.transaction()?;

        tx.execute("DELETE FROM scenes", [])?;
        tx.execute("DELETE FROM fixture_groups", [])?;
        tx.execute("DELETE FROM patched_fixtures", [])?;
        tx.execute("DELETE FROM ofl_fixtures", [])?;
        tx.execute("DELETE FROM user_views", [])?;
        tx.execute("DELETE FROM user_permission_overrides", [])?;
        tx.execute("DELETE FROM user_roles", [])?;
        tx.execute("DELETE FROM users", [])?;
        tx.execute("DELETE FROM roles", [])?;
        tx.execute("DELETE FROM server_identity", [])?;
        tx.execute("DELETE FROM show_meta", [])?;

        tx.execute(
            "INSERT INTO show_meta (key, value) VALUES ('show_title', ?1)",
            params![self.show_title],
        )?;

        if let Some(identity) = &self.server_identity {
            tx.execute(
                "INSERT INTO server_identity (id, cert_pem, key_pem) VALUES (1, ?1, ?2)",
                params![identity.cert_pem, identity.key_pem],
            )?;
        }

        for role in self.roles.values() {
            let perms_json = serde_json::to_string(&role.permissions)?;
            tx.execute(
                "INSERT INTO roles (id, name, permissions_json) VALUES (?1, ?2, ?3)",
                params![role.id, role.name, perms_json],
            )?;
        }

        for user in self.users.values() {
            tx.execute(
                "INSERT INTO users (id, username, password_hash, preferences_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    user.id,
                    user.username,
                    user.password_hash,
                    user.preferences_json
                ],
            )?;

            for role_id in &user.role_ids {
                tx.execute(
                    "INSERT INTO user_roles (user_id, role_id) VALUES (?1, ?2)",
                    params![user.id, role_id],
                )?;
            }

            for ov in &user.overrides {
                let ov_json = serde_json::to_string(ov)?;
                tx.execute(
                    "INSERT INTO user_permission_overrides (user_id, override_json) VALUES (?1, ?2)",
                    params![user.id, ov_json],
                )?;
            }

            for view in &user.views {
                tx.execute(
                    "INSERT INTO user_views (id, user_id, name, layout_json) VALUES (?1, ?2, ?3, ?4)",
                    params![view.id, user.id, view.name, view.layout_json],
                )?;
            }
        }

        for def in self.fixture_library.values() {
            let def_json = serde_json::to_string(def)?;
            tx.execute(
                "INSERT INTO ofl_fixtures (key, definition_json) VALUES (?1, ?2)",
                params![def.key, def_json],
            )?;
        }

        for fix in &self.patch {
            tx.execute(
                "INSERT INTO patched_fixtures (id, name, fixture_type_key, mode_name, universe, start_address) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    fix.id,
                    fix.name,
                    fix.fixture_type_key,
                    fix.mode_name,
                    fix.universe,
                    fix.start_address.get()
                ],
            )?;
        }

        for group in &self.groups {
            let ids_json = serde_json::to_string(&group.fixture_ids)?;
            tx.execute(
                "INSERT INTO fixture_groups (id, name, fixture_ids_json) VALUES (?1, ?2, ?3)",
                params![group.id, group.name, ids_json],
            )?;
        }

        for scene in &self.scenes {
            let vals_json = serde_json::to_string(&scene.values)?;
            tx.execute(
                "INSERT INTO scenes (id, name, fade_in_ms, fade_out_ms, values_json) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    scene.id,
                    scene.name,
                    scene.fade_in_ms,
                    scene.fade_out_ms,
                    vals_json
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn load_from_sqlite(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(GOBO_SQLITE_SCHEMA)?;

        let show_title = conn
            .query_row(
                "SELECT value FROM show_meta WHERE key = 'show_title'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| "Default Show".to_string());

        let server_identity = conn
            .query_row(
                "SELECT cert_pem, key_pem FROM server_identity WHERE id = 1",
                [],
                |row| {
                    Ok(ServerTlsIdentity {
                        cert_pem: row.get(0)?,
                        key_pem: row.get(1)?,
                    })
                },
            )
            .optional()?;

        let mut roles = BTreeMap::new();
        {
            let mut stmt = conn.prepare("SELECT id, name, permissions_json FROM roles")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (id, name, perms_json) = r?;
                let permissions = serde_json::from_str(&perms_json)?;
                roles.insert(id.clone(), Role { id, name, permissions });
            }
        }

        let mut users = BTreeMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT id, username, password_hash, preferences_json FROM users")?;
            let rows = stmt.query_map([], |row| {
                Ok(StoredUser {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    password_hash: row.get(2)?,
                    role_ids: Vec::new(),
                    overrides: Vec::new(),
                    views: Vec::new(),
                    preferences_json: row.get(3)?,
                })
            })?;
            for u in rows {
                let user = u?;
                users.insert(user.id.clone(), user);
            }
        }

        {
            let mut stmt = conn.prepare("SELECT user_id, role_id FROM user_roles")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (uid, rid) = r?;
                if let Some(u) = users.get_mut(&uid) {
                    u.role_ids.push(rid);
                }
            }
        }

        {
            let mut stmt = conn.prepare(
                "SELECT user_id, override_json FROM user_permission_overrides ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (uid, ov_json) = r?;
                let ov: PermissionOverride = serde_json::from_str(&ov_json)?;
                if let Some(u) = users.get_mut(&uid) {
                    u.overrides.push(ov);
                }
            }
        }

        {
            let mut stmt =
                conn.prepare("SELECT id, user_id, name, layout_json FROM user_views")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    UserView {
                        id: row.get(0)?,
                        name: row.get(2)?,
                        layout_json: row.get(3)?,
                    },
                ))
            })?;
            for r in rows {
                let (uid, view) = r?;
                if let Some(u) = users.get_mut(&uid) {
                    u.views.push(view);
                }
            }
        }

        let mut fixture_library = BTreeMap::new();
        {
            let mut stmt = conn.prepare("SELECT key, definition_json FROM ofl_fixtures")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for r in rows {
                let (key, def_json) = r?;
                let def: OflFixtureDefinition = serde_json::from_str(&def_json)?;
                fixture_library.insert(key, def);
            }
        }

        let mut patch = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, name, fixture_type_key, mode_name, universe, start_address FROM patched_fixtures",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u16>(4)?,
                    row.get::<_, u16>(5)?,
                ))
            })?;
            for r in rows {
                let (id, name, fixture_type_key, mode_name, universe, addr_raw) = r?;
                let start_address =
                    DmxAddress::new(addr_raw).ok_or(StorageError::InvalidDmxAddress(addr_raw))?;
                patch.push(PatchedFixture {
                    id,
                    name,
                    fixture_type_key,
                    mode_name,
                    universe,
                    start_address,
                });
            }
        }

        let mut groups = Vec::new();
        {
            let mut stmt =
                conn.prepare("SELECT id, name, fixture_ids_json FROM fixture_groups")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for r in rows {
                let (id, name, ids_json) = r?;
                let fixture_ids = serde_json::from_str(&ids_json)?;
                groups.push(FixtureGroup {
                    id,
                    name,
                    fixture_ids,
                });
            }
        }

        let mut scenes = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, name, fade_in_ms, fade_out_ms, values_json FROM scenes",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            for r in rows {
                let (id, name, fade_in_ms, fade_out_ms, vals_json) = r?;
                let values = serde_json::from_str(&vals_json)?;
                scenes.push(Scene {
                    id,
                    name,
                    fade_in_ms,
                    fade_out_ms,
                    values,
                });
            }
        }

        Ok(Self {
            show_title,
            server_identity,
            roles,
            users,
            fixture_library,
            patch,
            groups,
            scenes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opengobo_protocol::{AccessLevel, AttributeKind, NormalizedValue, Permission, PermissionEffect, Resource};

    #[test]
    fn sqlite_gobo_roundtrip_and_magic_header_work() {
        let tmp_path = std::env::temp_dir().join("opengobo_sqlite_test.gobo");
        let _ = fs::remove_file(&tmp_path);

        let mut doc = ShowDocument::new("SQLite Arena Show");
        doc.server_identity = Some(ServerTlsIdentity {
            cert_pem: "CERT".to_string(),
            key_pem: "KEY".to_string(),
        });
        doc.users.insert(
            "u-admin".to_string(),
            StoredUser {
                id: "u-admin".to_string(),
                username: "admin".to_string(),
                password_hash: "$argon2id$v=19$m=19456,t=2,p=1$salt$hash".to_string(),
                role_ids: vec!["admin".to_string()],
                overrides: vec![PermissionOverride {
                    effect: PermissionEffect::Grant,
                    permission: Permission::specific(Resource::Scene, AccessLevel::Write, "s1"),
                }],
                views: vec![UserView {
                    id: "v1".to_string(),
                    name: "Main".to_string(),
                    layout_json: "{}".to_string(),
                }],
                preferences_json: "{\"theme\":\"dark\"}".to_string(),
            },
        );
        doc.patch.push(PatchedFixture {
            id: "f1".to_string(),
            name: "Front PAR".to_string(),
            fixture_type_key: "generic/dimmer-rgb-par".to_string(),
            mode_name: "4ch".to_string(),
            universe: 1,
            start_address: DmxAddress::new(1).unwrap_or_else(|| panic!("addr")),
        });
        doc.groups.push(FixtureGroup {
            id: "g1".to_string(),
            name: "Front".to_string(),
            fixture_ids: vec!["f1".to_string()],
        });
        doc.scenes.push(Scene {
            id: "s1".to_string(),
            name: "Look 1".to_string(),
            fade_in_ms: 500,
            fade_out_ms: 1000,
            values: BTreeMap::from([(
                "f1".to_string(),
                BTreeMap::from([(AttributeKind::Dimmer, NormalizedValue::FULL)]),
            )]),
        });

        doc.save(&tmp_path).unwrap_or_else(|e| panic!("save: {e}"));

        // Verify file on disk has real SQLite 3 binary header ("SQLite format 3\0").
        let raw_bytes = fs::read(&tmp_path).unwrap_or_else(|e| panic!("read: {e}"));
        assert!(raw_bytes.starts_with(b"SQLite format 3\0"));

        let loaded =
            ShowDocument::load_or_create(&tmp_path).unwrap_or_else(|e| panic!("load: {e}"));
        assert_eq!(loaded, doc);

        let perms = loaded
            .effective_permissions_for("u-admin")
            .unwrap_or_else(|| panic!("user should exist"));
        assert!(perms.is_allowed(Resource::System, AccessLevel::Write, None));

        let _ = fs::remove_file(&tmp_path);
    }
}
