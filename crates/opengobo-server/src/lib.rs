use opengobo_engine::{EngineError, ShowEngine};
use opengobo_protocol::{
    ClientCommand, ClientPacket, PROTOCOL_VERSION, ServerHello, ServerPacket, StateDelta, UserId,
};
use opengobo_storage::{ShowDocument, StorageError, StoredUser};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("engine error: {0}")]
    Engine(#[from] EngineError),
}

/// Coordinates the loaded `.gobo` showfile, Setup-PIN, sessions, and real-time [`ShowEngine`].
#[derive(Debug)]
pub struct CoreCoordinator {
    show_path: Option<PathBuf>,
    document: ShowDocument,
    engine: ShowEngine,
    setup_pin: Option<String>,
    sessions: BTreeMap<String, UserId>,
}

impl CoreCoordinator {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ServerError> {
        let path_buf = path.as_ref().to_path_buf();
        let document = ShowDocument::load_or_create(&path_buf)?;
        let mut coord = Self::in_memory(document, None)?;
        coord.show_path = Some(path_buf);
        Ok(coord)
    }

    pub fn in_memory(
        document: ShowDocument,
        setup_pin: Option<String>,
    ) -> Result<Self, ServerError> {
        let engine = ShowEngine::from_document(&document)?;
        let pin = document
            .is_setup_mode_required()
            .then(|| setup_pin.unwrap_or_else(Self::generate_setup_pin));

        Ok(Self {
            show_path: None,
            document,
            engine,
            setup_pin: pin,
            sessions: BTreeMap::new(),
        })
    }

    fn generate_setup_pin() -> String {
        ((Uuid::new_v4().as_u128() % 900_000) + 100_000).to_string()
    }

    #[must_use]
    pub fn setup_pin(&self) -> Option<&str> {
        self.setup_pin.as_deref()
    }

    #[must_use]
    pub fn server_hello(&self) -> ServerHello {
        ServerHello {
            protocol_version: PROTOCOL_VERSION,
            show_title: self.document.show_title.clone(),
            setup_mode_required: self.document.is_setup_mode_required(),
        }
    }

    #[must_use]
    pub const fn engine(&self) -> &ShowEngine {
        &self.engine
    }

    pub fn handle_client_packet(
        &mut self,
        authenticated_user: &mut Option<UserId>,
        packet: ClientPacket,
    ) -> (ServerPacket, Option<StateDelta>) {
        match packet {
            ClientPacket::CompleteSetup {
                setup_pin,
                admin_username,
                admin_password,
            } => (
                self.handle_complete_setup(
                    authenticated_user,
                    &setup_pin,
                    &admin_username,
                    &admin_password,
                ),
                None,
            ),
            ClientPacket::LoginPassword { username, password } => (
                self.handle_login_password(authenticated_user, &username, &password),
                None,
            ),
            ClientPacket::LoginToken { session_token } => (
                self.handle_login_token(authenticated_user, &session_token),
                None,
            ),
            ClientPacket::Logout => {
                if let Some(uid) = authenticated_user.take() {
                    self.engine.clear_programmer_on_logout(&uid);
                }
                (
                    ServerPacket::CommandResponse {
                        request_id: 0,
                        accepted: true,
                        error: None,
                    },
                    None,
                )
            }
            ClientPacket::Command {
                request_id,
                command,
            } => self.handle_authenticated_command(
                authenticated_user.as_deref(),
                request_id,
                command,
            ),
        }
    }

    fn handle_complete_setup(
        &mut self,
        authenticated_user: &mut Option<UserId>,
        provided_pin: &str,
        admin_username: &str,
        admin_password: &str,
    ) -> ServerPacket {
        if !self.document.is_setup_mode_required() {
            return ServerPacket::AuthError {
                message: "Setup mode is already completed".to_string(),
            };
        }
        if self.setup_pin.as_deref() != Some(provided_pin.trim()) {
            return ServerPacket::AuthError {
                message: "Invalid Setup PIN".to_string(),
            };
        }
        let username = admin_username.trim();
        if username.is_empty() || admin_password.is_empty() {
            return ServerPacket::AuthError {
                message: "Username and password must not be empty".to_string(),
            };
        }

        let user_id = Uuid::new_v4().to_string();
        self.document.users.insert(
            user_id.clone(),
            StoredUser {
                id: user_id.clone(),
                username: username.to_string(),
                password_hash: Self::hash_secret(admin_password),
                role_ids: vec!["admin".to_string()],
                overrides: Vec::new(),
                views: Vec::new(),
                preferences_json: "{}".to_string(),
            },
        );
        self.setup_pin = None;
        self.persist_document();

        self.issue_auth_success(authenticated_user, &user_id)
    }

    fn handle_login_password(
        &mut self,
        authenticated_user: &mut Option<UserId>,
        username: &str,
        password: &str,
    ) -> ServerPacket {
        let password_hash = Self::hash_secret(password);
        let found_id = self
            .document
            .users
            .values()
            .find(|u| u.username == username && u.password_hash == password_hash)
            .map(|u| u.id.clone());

        match found_id {
            Some(user_id) => self.issue_auth_success(authenticated_user, &user_id),
            None => ServerPacket::AuthError {
                message: "Invalid credentials".to_string(),
            },
        }
    }

    fn handle_login_token(
        &mut self,
        authenticated_user: &mut Option<UserId>,
        session_token: &str,
    ) -> ServerPacket {
        if let Some(user_id) = self.sessions.get(session_token).cloned()
            && let Some(snapshot) = self.build_snapshot(&user_id)
        {
            *authenticated_user = Some(user_id);
            return ServerPacket::AuthSuccess {
                session_token: session_token.to_string(),
                snapshot: Box::new(snapshot),
            };
        }
        ServerPacket::AuthError {
            message: "Invalid or expired session token".to_string(),
        }
    }

    fn issue_auth_success(
        &mut self,
        authenticated_user: &mut Option<UserId>,
        user_id: &str,
    ) -> ServerPacket {
        let Some(snapshot) = self.build_snapshot(user_id) else {
            return ServerPacket::AuthError {
                message: "User profile could not be loaded".to_string(),
            };
        };
        let token = Uuid::new_v4().to_string();
        self.sessions.insert(token.clone(), user_id.to_string());
        *authenticated_user = Some(user_id.to_string());
        ServerPacket::AuthSuccess {
            session_token: token,
            snapshot: Box::new(snapshot),
        }
    }

    fn build_snapshot(&self, user_id: &str) -> Option<opengobo_protocol::ShowSnapshot> {
        let user = self.document.users.get(user_id)?.to_profile();
        let perms = self.document.effective_permissions_for(user_id)?;
        Some(self.engine.snapshot_for_user(user, perms))
    }

    fn handle_authenticated_command(
        &mut self,
        authenticated_user: Option<&str>,
        request_id: u64,
        command: ClientCommand,
    ) -> (ServerPacket, Option<StateDelta>) {
        let Some(user_id) = authenticated_user else {
            return (
                ServerPacket::CommandResponse {
                    request_id,
                    accepted: false,
                    error: Some("Unauthenticated connection".to_string()),
                },
                None,
            );
        };
        let Some(perms) = self.document.effective_permissions_for(user_id) else {
            return (
                ServerPacket::CommandResponse {
                    request_id,
                    accepted: false,
                    error: Some("User permissions not found".to_string()),
                },
                None,
            );
        };

        match self
            .engine
            .execute_command(user_id, &perms, command.clone())
        {
            Ok(delta) => {
                self.sync_structural_command_to_document(user_id, command);
                (
                    ServerPacket::CommandResponse {
                        request_id,
                        accepted: true,
                        error: None,
                    },
                    Some(delta),
                )
            }
            Err(err) => (
                ServerPacket::CommandResponse {
                    request_id,
                    accepted: false,
                    error: Some(err.to_string()),
                },
                None,
            ),
        }
    }

    fn sync_structural_command_to_document(&mut self, user_id: &str, command: ClientCommand) {
        match command {
            ClientCommand::UpsertFixture { fixture } => {
                upsert_vec(&mut self.document.patch, fixture, |f| &f.id);
            }
            ClientCommand::RemoveFixture { fixture_id } => {
                self.document.patch.retain(|f| f.id != fixture_id);
            }
            ClientCommand::UpsertGroup { group } => {
                upsert_vec(&mut self.document.groups, group, |g| &g.id);
            }
            ClientCommand::DeleteGroup { group_id } => {
                self.document.groups.retain(|g| g.id != group_id);
            }
            ClientCommand::UpsertScene { scene } => {
                upsert_vec(&mut self.document.scenes, scene, |s| &s.id);
            }
            ClientCommand::DeleteScene { scene_id } => {
                self.document.scenes.retain(|s| s.id != scene_id);
            }
            ClientCommand::SaveUserView { view } => {
                if let Some(user) = self.document.users.get_mut(user_id) {
                    upsert_vec(&mut user.views, view, |v| &v.id);
                }
            }
            _ => return,
        }
        self.persist_document();
    }

    fn persist_document(&self) {
        if let Some(path) = &self.show_path
            && let Err(err) = self.document.save(path)
        {
            tracing::error!("Failed to auto-save showfile {}: {err}", path.display());
        }
    }

    fn hash_secret(secret: &str) -> String {
        format!("opengobo-auth-v1:{secret}")
    }
}

fn upsert_vec<T>(items: &mut Vec<T>, item: T, id_fn: impl Fn(&T) -> &str) {
    let target_id = id_fn(&item);
    items.retain(|existing| id_fn(existing) != target_id);
    items.push(item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opengobo_protocol::NormalizedValue;

    #[test]
    fn setup_pin_handshake_and_authenticated_command_flow() {
        let doc = ShowDocument::new("Club Night");
        let mut coord = CoreCoordinator::in_memory(doc, Some("123456".to_string()))
            .unwrap_or_else(|e| panic!("coordinator init: {e}"));

        assert!(coord.server_hello().setup_mode_required);
        let mut conn_user: Option<UserId> = None;

        let (resp, delta) = coord.handle_client_packet(
            &mut conn_user,
            ClientPacket::Command {
                request_id: 1,
                command: ClientCommand::SetBlackout { active: true },
            },
        );
        assert!(delta.is_none());
        assert!(matches!(
            resp,
            ServerPacket::CommandResponse {
                accepted: false,
                ..
            }
        ));

        let (resp, _) = coord.handle_client_packet(
            &mut conn_user,
            ClientPacket::CompleteSetup {
                setup_pin: "000000".to_string(),
                admin_username: "mika".to_string(),
                admin_password: "4242".to_string(),
            },
        );
        assert!(matches!(resp, ServerPacket::AuthError { .. }));

        let (resp, _) = coord.handle_client_packet(
            &mut conn_user,
            ClientPacket::CompleteSetup {
                setup_pin: "123456".to_string(),
                admin_username: "mika".to_string(),
                admin_password: "4242".to_string(),
            },
        );
        assert!(matches!(resp, ServerPacket::AuthSuccess { .. }));
        assert!(!coord.server_hello().setup_mode_required);
        assert!(conn_user.is_some());

        let (resp, delta) = coord.handle_client_packet(
            &mut conn_user,
            ClientPacket::Command {
                request_id: 2,
                command: ClientCommand::SetGrandMaster {
                    value: NormalizedValue::new(0.75),
                },
            },
        );
        assert!(matches!(
            resp,
            ServerPacket::CommandResponse { accepted: true, .. }
        ));
        assert!(matches!(delta, Some(StateDelta::GrandMasterChanged { .. })));
    }
}
