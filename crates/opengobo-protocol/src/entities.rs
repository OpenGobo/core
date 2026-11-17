use crate::attributes::{AttributeKind, FixtureAttributeMap, NormalizedValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type UniverseId = u16;
pub type FixtureId = String;
pub type GroupId = String;
pub type SceneId = String;
pub type UserId = String;

/// 1-based DMX start address (`1..=512`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DmxAddress(u16);

impl DmxAddress {
    #[must_use]
    pub fn new(address: u16) -> Option<Self> {
        (1..=512).contains(&address).then_some(Self(address))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn zero_based_index(self) -> usize {
        (self.0 - 1) as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchedFixture {
    pub id: FixtureId,
    pub name: String,
    pub fixture_type_key: String,
    pub mode_name: String,
    pub universe: UniverseId,
    pub start_address: DmxAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureGroup {
    pub id: GroupId,
    pub name: String,
    pub fixture_ids: Vec<FixtureId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    pub values: BTreeMap<FixtureId, FixtureAttributeMap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProgrammerEntry {
    pub value: NormalizedValue,
    /// Monotonic sequence number for multi-user LTP resolution.
    pub updated_at_seq: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProgrammerState {
    /// When `true`, programmer values are excluded from live stage mixing.
    pub blind: bool,
    pub selected_fixtures: Vec<FixtureId>,
    pub attributes: BTreeMap<FixtureId, BTreeMap<AttributeKind, ProgrammerEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserView {
    pub id: String,
    pub name: String,
    pub layout_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: UserId,
    pub username: String,
    pub roles: Vec<String>,
    pub views: Vec<UserView>,
    pub preferences_json: String,
}
