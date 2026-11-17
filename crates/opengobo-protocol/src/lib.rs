pub mod attributes;
pub mod entities;
pub mod messages;
pub mod permissions;

pub use attributes::{AttributeKind, FixtureAttributeMap, NormalizedValue};
pub use entities::{
    DmxAddress, FixtureGroup, FixtureId, GroupId, PatchedFixture, ProgrammerEntry, ProgrammerState,
    Scene, SceneId, UniverseId, UserId, UserProfile, UserView,
};
pub use messages::{
    ClientCommand, ClientPacket, RealtimeDatagram, ServerHello, ServerPacket, ShowSnapshot,
    StateDelta,
};
pub use permissions::{
    AccessLevel, EffectivePermissions, Permission, PermissionEffect, PermissionOverride, Resource,
    Role, Scope,
};

pub const PROTOCOL_VERSION: u32 = 1;
