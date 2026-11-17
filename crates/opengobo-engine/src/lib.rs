use opengobo_fixture::{DMX_UNIVERSE_SIZE, FixtureError, FixtureProfile, OflFixtureDefinition};
use opengobo_protocol::{
    AccessLevel, AttributeKind, ClientCommand, EffectivePermissions, FixtureAttributeMap,
    FixtureGroup, FixtureId, GroupId, NormalizedValue, PatchedFixture, ProgrammerEntry,
    ProgrammerState, Resource, Scene, SceneId, ShowSnapshot, StateDelta, UniverseId, UserId,
    UserProfile,
};
use opengobo_storage::ShowDocument;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("permission denied: requires {level:?} on {resource:?} (target: {target:?})")]
    PermissionDenied {
        resource: Resource,
        level: AccessLevel,
        target: Option<String>,
    },
    #[error("unknown fixture definition '{0}'")]
    UnknownFixtureType(String),
    #[error("fixture compilation failed: {0}")]
    Fixture(#[from] FixtureError),
    #[error("entity '{0}' not found")]
    NotFound(String),
}

/// Real-time in-memory show state and HTP/LTP mixer.
#[derive(Debug, Clone)]
pub struct ShowEngine {
    pub show_title: String,
    pub grand_master: NormalizedValue,
    pub blackout: bool,
    fixture_library: BTreeMap<String, OflFixtureDefinition>,
    compiled_profiles: BTreeMap<(String, String), FixtureProfile>,
    patch: BTreeMap<FixtureId, PatchedFixture>,
    groups: BTreeMap<GroupId, FixtureGroup>,
    scenes: BTreeMap<SceneId, Scene>,
    active_playbacks: BTreeMap<SceneId, NormalizedValue>,
    programmers: BTreeMap<UserId, ProgrammerState>,
    sequence_counter: u64,
}

impl ShowEngine {
    pub fn from_document(doc: &ShowDocument) -> Result<Self, EngineError> {
        let mut engine = Self {
            show_title: doc.show_title.clone(),
            grand_master: NormalizedValue::FULL,
            blackout: false,
            fixture_library: doc.fixture_library.clone(),
            compiled_profiles: BTreeMap::new(),
            patch: BTreeMap::new(),
            groups: doc
                .groups
                .iter()
                .map(|g| (g.id.clone(), g.clone()))
                .collect(),
            scenes: doc
                .scenes
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect(),
            active_playbacks: BTreeMap::new(),
            programmers: BTreeMap::new(),
            sequence_counter: 0,
        };

        for fixture in &doc.patch {
            engine.insert_patched_fixture(fixture.clone())?;
        }

        Ok(engine)
    }

    fn insert_patched_fixture(&mut self, fixture: PatchedFixture) -> Result<(), EngineError> {
        let key = (fixture.fixture_type_key.clone(), fixture.mode_name.clone());
        if !self.compiled_profiles.contains_key(&key) {
            let def = self
                .fixture_library
                .get(&fixture.fixture_type_key)
                .ok_or_else(|| EngineError::UnknownFixtureType(fixture.fixture_type_key.clone()))?;
            self.compiled_profiles
                .insert(key, def.compile_mode(&fixture.mode_name)?);
        }
        self.patch.insert(fixture.id.clone(), fixture);
        Ok(())
    }

    /// Builds a [`ShowSnapshot`] filtered by the user's `View` permissions.
    #[must_use]
    pub fn snapshot_for_user(
        &self,
        user: UserProfile,
        permissions: EffectivePermissions,
    ) -> ShowSnapshot {
        let patch = if permissions.is_allowed(Resource::Patch, AccessLevel::View, None) {
            self.patch.values().cloned().collect()
        } else {
            Vec::new()
        };

        let groups = self
            .groups
            .values()
            .filter(|g| permissions.is_allowed(Resource::Group, AccessLevel::View, Some(&g.id)))
            .cloned()
            .collect();

        let scenes = self
            .scenes
            .values()
            .filter(|s| permissions.is_allowed(Resource::Scene, AccessLevel::View, Some(&s.id)))
            .cloned()
            .collect();

        let active_playbacks = self
            .active_playbacks
            .iter()
            .filter(|(id, _)| permissions.is_allowed(Resource::Scene, AccessLevel::View, Some(id)))
            .map(|(k, v)| (k.clone(), *v))
            .collect();

        let own_programmer = self.programmers.get(&user.id).cloned().unwrap_or_default();

        ShowSnapshot {
            show_title: self.show_title.clone(),
            user,
            permissions,
            grand_master: self.grand_master,
            blackout: self.blackout,
            patch,
            groups,
            scenes,
            active_playbacks,
            own_programmer,
        }
    }

    fn check(
        permissions: &EffectivePermissions,
        resource: Resource,
        level: AccessLevel,
        target: Option<&str>,
    ) -> Result<(), EngineError> {
        if permissions.is_allowed(resource, level, target) {
            Ok(())
        } else {
            Err(EngineError::PermissionDenied {
                resource,
                level,
                target: target.map(ToOwned::to_owned),
            })
        }
    }

    pub fn execute_command(
        &mut self,
        user_id: &str,
        permissions: &EffectivePermissions,
        command: ClientCommand,
    ) -> Result<StateDelta, EngineError> {
        match command {
            ClientCommand::SetGrandMaster { value } => {
                Self::check(permissions, Resource::System, AccessLevel::Control, None)?;
                self.grand_master = value;
                Ok(StateDelta::GrandMasterChanged { value })
            }
            ClientCommand::SetBlackout { active } => {
                Self::check(permissions, Resource::System, AccessLevel::Control, None)?;
                self.blackout = active;
                Ok(StateDelta::BlackoutChanged { active })
            }
            ClientCommand::UpsertFixture { fixture } => {
                Self::check(permissions, Resource::Patch, AccessLevel::Write, None)?;
                self.insert_patched_fixture(fixture.clone())?;
                Ok(StateDelta::FixtureUpserted { fixture })
            }
            ClientCommand::RemoveFixture { fixture_id } => {
                Self::check(permissions, Resource::Patch, AccessLevel::Write, None)?;
                self.patch.remove(&fixture_id);
                Ok(StateDelta::FixtureRemoved { fixture_id })
            }
            ClientCommand::UpsertGroup { group } => {
                Self::check(
                    permissions,
                    Resource::Group,
                    AccessLevel::Write,
                    Some(&group.id),
                )?;
                self.groups.insert(group.id.clone(), group.clone());
                Ok(StateDelta::GroupUpserted { group })
            }
            ClientCommand::DeleteGroup { group_id } => {
                Self::check(
                    permissions,
                    Resource::Group,
                    AccessLevel::Write,
                    Some(&group_id),
                )?;
                self.groups.remove(&group_id);
                Ok(StateDelta::GroupDeleted { group_id })
            }
            ClientCommand::UpsertScene { scene } => {
                Self::check(
                    permissions,
                    Resource::Scene,
                    AccessLevel::Write,
                    Some(&scene.id),
                )?;
                self.scenes.insert(scene.id.clone(), scene.clone());
                Ok(StateDelta::SceneUpserted { scene })
            }
            ClientCommand::DeleteScene { scene_id } => {
                Self::check(
                    permissions,
                    Resource::Scene,
                    AccessLevel::Write,
                    Some(&scene_id),
                )?;
                self.scenes.remove(&scene_id);
                self.active_playbacks.remove(&scene_id);
                Ok(StateDelta::SceneDeleted { scene_id })
            }
            ClientCommand::SetScenePlayback { scene_id, level } => {
                Self::check(
                    permissions,
                    Resource::Scene,
                    AccessLevel::Control,
                    Some(&scene_id),
                )?;
                if !self.scenes.contains_key(&scene_id) {
                    return Err(EngineError::NotFound(scene_id));
                }
                if level.get() <= 0.0 {
                    self.active_playbacks.remove(&scene_id);
                } else {
                    self.active_playbacks.insert(scene_id.clone(), level);
                }
                Ok(StateDelta::ScenePlaybackChanged { scene_id, level })
            }
            ClientCommand::StopScene { scene_id } => {
                Self::check(
                    permissions,
                    Resource::Scene,
                    AccessLevel::Control,
                    Some(&scene_id),
                )?;
                self.active_playbacks.remove(&scene_id);
                Ok(StateDelta::ScenePlaybackChanged {
                    scene_id,
                    level: NormalizedValue::ZERO,
                })
            }
            ClientCommand::SetProgrammerBlind { blind } => {
                Self::check(
                    permissions,
                    Resource::Programmer,
                    AccessLevel::Control,
                    None,
                )?;
                let prog = self.programmers.entry(user_id.to_string()).or_default();
                prog.blind = blind;
                Ok(StateDelta::ProgrammerUpdated {
                    user_id: user_id.to_string(),
                    programmer: prog.clone(),
                })
            }
            ClientCommand::SetProgrammerAttribute {
                fixture_id,
                attribute,
                value,
            } => {
                Self::check(
                    permissions,
                    Resource::Programmer,
                    AccessLevel::Control,
                    None,
                )?;
                self.sequence_counter = self.sequence_counter.saturating_add(1);
                let seq = self.sequence_counter;
                let prog = self.programmers.entry(user_id.to_string()).or_default();
                prog.attributes.entry(fixture_id).or_default().insert(
                    attribute,
                    ProgrammerEntry {
                        value,
                        updated_at_seq: seq,
                    },
                );
                Ok(StateDelta::ProgrammerUpdated {
                    user_id: user_id.to_string(),
                    programmer: prog.clone(),
                })
            }
            ClientCommand::ClearProgrammer { target_user } => {
                let target_id = target_user.as_deref().unwrap_or(user_id);
                let level = if target_id == user_id {
                    AccessLevel::Control
                } else {
                    AccessLevel::Write
                };
                Self::check(permissions, Resource::Programmer, level, None)?;
                let prog = self.programmers.entry(target_id.to_string()).or_default();
                prog.attributes.clear();
                prog.selected_fixtures.clear();
                Ok(StateDelta::ProgrammerUpdated {
                    user_id: target_id.to_string(),
                    programmer: prog.clone(),
                })
            }
            ClientCommand::SaveUserView { view } => Ok(StateDelta::ProgrammerUpdated {
                user_id: format!("{user_id}:view:{}", view.id),
                programmer: self.programmers.get(user_id).cloned().unwrap_or_default(),
            }),
        }
    }

    pub fn clear_programmer_on_logout(&mut self, user_id: &str) {
        self.programmers.remove(user_id);
    }

    #[must_use]
    pub fn render_universes(&self) -> BTreeMap<UniverseId, [u8; DMX_UNIVERSE_SIZE]> {
        let mut universes = BTreeMap::new();

        for fixture in self.patch.values() {
            let universe_buf = universes
                .entry(fixture.universe)
                .or_insert([0u8; DMX_UNIVERSE_SIZE]);
            let mixed_attrs = self.mix_fixture_attributes(&fixture.id);
            let key = (fixture.fixture_type_key.clone(), fixture.mode_name.clone());

            if let Some(profile) = self.compiled_profiles.get(&key) {
                let _ =
                    profile.render_into_universe(fixture.start_address, &mixed_attrs, universe_buf);
            }
        }

        universes
    }

    fn mix_fixture_attributes(&self, fixture_id: &str) -> FixtureAttributeMap {
        let mut mixed = FixtureAttributeMap::new();

        // 1. Active scenes: HTP for Dimmer (scaled by playback level), LTP for other attributes.
        for (scene_id, &level) in &self.active_playbacks {
            let Some(fixture_vals) = self
                .scenes
                .get(scene_id)
                .and_then(|s| s.values.get(fixture_id))
            else {
                continue;
            };
            for (attr, &val) in fixture_vals {
                if attr.is_htp() {
                    let scaled = NormalizedValue::new(val.get() * level.get());
                    let prev = mixed.get(attr).copied().unwrap_or(NormalizedValue::ZERO);
                    if scaled.get() > prev.get() {
                        mixed.insert(attr.clone(), scaled);
                    }
                } else {
                    mixed.insert(attr.clone(), val);
                }
            }
        }

        // 2. Non-blind programmers: LTP ordered by `updated_at_seq`.
        let mut prog_winners: BTreeMap<AttributeKind, ProgrammerEntry> = BTreeMap::new();
        for prog in self.programmers.values().filter(|p| !p.blind) {
            let Some(overrides) = prog.attributes.get(fixture_id) else {
                continue;
            };
            for (attr, &entry) in overrides {
                if prog_winners
                    .get(attr)
                    .is_none_or(|prev| entry.updated_at_seq >= prev.updated_at_seq)
                {
                    prog_winners.insert(attr.clone(), entry);
                }
            }
        }
        for (attr, winner) in prog_winners {
            mixed.insert(attr, winner.value);
        }

        // 3. Apply Grand Master and Blackout to Dimmer.
        if let Some(dimmer) = mixed.get_mut(&AttributeKind::Dimmer) {
            *dimmer = if self.blackout {
                NormalizedValue::ZERO
            } else {
                NormalizedValue::new(dimmer.get() * self.grand_master.get())
            };
        }

        mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opengobo_protocol::{DmxAddress, Role};

    #[test]
    fn htp_scene_mixing_and_multi_user_ltp_programmer_work() {
        let doc = ShowDocument::new("Test Show");
        let mut engine =
            ShowEngine::from_document(&doc).unwrap_or_else(|e| panic!("engine init: {e}"));
        let admin_perms = EffectivePermissions {
            role_permissions: Role::admin().permissions,
            user_overrides: Vec::new(),
        };

        let fixture = PatchedFixture {
            id: "par-1".to_string(),
            name: "Front PAR 1".to_string(),
            fixture_type_key: "generic/dimmer-rgb-par".to_string(),
            mode_name: "4ch".to_string(),
            universe: 1,
            start_address: DmxAddress::new(1).unwrap_or_else(|| panic!("valid address")),
        };
        assert!(
            engine
                .execute_command(
                    "op-1",
                    &admin_perms,
                    ClientCommand::UpsertFixture { fixture }
                )
                .is_ok()
        );

        let s1_vals = BTreeMap::from([(
            "par-1".to_string(),
            FixtureAttributeMap::from([
                (AttributeKind::Dimmer, NormalizedValue::new(0.4)),
                (AttributeKind::ColorRed, NormalizedValue::FULL),
            ]),
        )]);
        let s2_vals = BTreeMap::from([(
            "par-1".to_string(),
            FixtureAttributeMap::from([(AttributeKind::Dimmer, NormalizedValue::new(0.8))]),
        )]);

        for (id, values) in [("s1", s1_vals), ("s2", s2_vals)] {
            let _ = engine.execute_command(
                "op-1",
                &admin_perms,
                ClientCommand::UpsertScene {
                    scene: Scene {
                        id: id.to_string(),
                        name: id.to_string(),
                        fade_in_ms: 0,
                        fade_out_ms: 0,
                        values,
                    },
                },
            );
            let _ = engine.execute_command(
                "op-1",
                &admin_perms,
                ClientCommand::SetScenePlayback {
                    scene_id: id.to_string(),
                    level: NormalizedValue::FULL,
                },
            );
        }

        let dmx = engine.render_universes();
        let u1 = dmx.get(&1).unwrap_or_else(|| panic!("universe 1 present"));
        assert_eq!(&u1[0..2], &[204, 255]);

        let _ = engine.execute_command(
            "op-1",
            &admin_perms,
            ClientCommand::SetProgrammerAttribute {
                fixture_id: "par-1".to_string(),
                attribute: AttributeKind::ColorBlue,
                value: NormalizedValue::new(0.5),
            },
        );
        let _ = engine.execute_command(
            "op-2",
            &admin_perms,
            ClientCommand::SetProgrammerAttribute {
                fixture_id: "par-1".to_string(),
                attribute: AttributeKind::ColorBlue,
                value: NormalizedValue::FULL,
            },
        );

        let dmx = engine.render_universes();
        assert_eq!(dmx.get(&1).unwrap_or_else(|| panic!("u1"))[3], 255);

        let _ = engine.execute_command(
            "op-2",
            &admin_perms,
            ClientCommand::SetProgrammerBlind { blind: true },
        );
        let dmx = engine.render_universes();
        assert_eq!(dmx.get(&1).unwrap_or_else(|| panic!("u1"))[3], 128);
    }
}
