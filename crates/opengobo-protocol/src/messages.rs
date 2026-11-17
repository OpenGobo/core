use crate::attributes::{AttributeKind, NormalizedValue};
use crate::entities::{
    FixtureGroup, FixtureId, GroupId, PatchedFixture, ProgrammerState, Scene, SceneId, UniverseId,
    UserId, UserProfile, UserView,
};
use crate::permissions::EffectivePermissions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Initial greeting sent before authentication (Zero-User-Enumeration: never lists usernames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol_version: u32,
    pub show_title: String,
    /// `true` when the `.gobo` file has no users yet and expects a Setup-PIN + initial admin.
    pub setup_mode_required: bool,
}

/// Show state snapshot filtered by the user's `View` permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShowSnapshot {
    pub show_title: String,
    pub user: UserProfile,
    pub permissions: EffectivePermissions,
    pub grand_master: NormalizedValue,
    pub blackout: bool,
    pub patch: Vec<PatchedFixture>,
    pub groups: Vec<FixtureGroup>,
    pub scenes: Vec<Scene>,
    pub active_playbacks: BTreeMap<SceneId, NormalizedValue>,
    pub own_programmer: ProgrammerState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum ClientCommand {
    SetGrandMaster {
        value: NormalizedValue,
    },
    SetBlackout {
        active: bool,
    },
    UpsertFixture {
        fixture: PatchedFixture,
    },
    RemoveFixture {
        fixture_id: FixtureId,
    },
    UpsertGroup {
        group: FixtureGroup,
    },
    DeleteGroup {
        group_id: GroupId,
    },
    UpsertScene {
        scene: Scene,
    },
    DeleteScene {
        scene_id: SceneId,
    },
    SetScenePlayback {
        scene_id: SceneId,
        level: NormalizedValue,
    },
    StopScene {
        scene_id: SceneId,
    },
    SetProgrammerBlind {
        blind: bool,
    },
    SetProgrammerAttribute {
        fixture_id: FixtureId,
        attribute: AttributeKind,
        value: NormalizedValue,
    },
    ClearProgrammer {
        target_user: Option<UserId>,
    },
    SaveUserView {
        view: UserView,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "delta")]
pub enum StateDelta {
    GrandMasterChanged {
        value: NormalizedValue,
    },
    BlackoutChanged {
        active: bool,
    },
    FixtureUpserted {
        fixture: PatchedFixture,
    },
    FixtureRemoved {
        fixture_id: FixtureId,
    },
    GroupUpserted {
        group: FixtureGroup,
    },
    GroupDeleted {
        group_id: GroupId,
    },
    SceneUpserted {
        scene: Scene,
    },
    SceneDeleted {
        scene_id: SceneId,
    },
    ScenePlaybackChanged {
        scene_id: SceneId,
        level: NormalizedValue,
    },
    ProgrammerUpdated {
        user_id: UserId,
        programmer: ProgrammerState,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClientPacket {
    CompleteSetup {
        setup_pin: String,
        admin_username: String,
        admin_password: String,
    },
    LoginPassword {
        username: String,
        password: String,
    },
    LoginToken {
        session_token: String,
    },
    Logout,
    Command {
        request_id: u64,
        command: ClientCommand,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerPacket {
    Hello(ServerHello),
    AuthSuccess {
        session_token: String,
        snapshot: Box<ShowSnapshot>,
    },
    AuthError {
        message: String,
    },
    CommandResponse {
        request_id: u64,
        accepted: bool,
        error: Option<String>,
    },
    Delta(StateDelta),
}

/// Unreliable QUIC datagram payload for high-rate real-time faders, encoders, and DMX telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RealtimeDatagram {
    SceneFaderMove {
        scene_id: SceneId,
        level: NormalizedValue,
    },
    ProgrammerEncoderMove {
        fixture_id: FixtureId,
        attribute: AttributeKind,
        value: NormalizedValue,
    },
    DmxUniverseFrame {
        universe: UniverseId,
        channels: Vec<u8>,
    },
}
