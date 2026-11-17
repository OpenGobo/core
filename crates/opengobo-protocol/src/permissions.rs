use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    System,
    Users,
    Patch,
    Programmer,
    Group,
    Scene,
}

/// Access hierarchy (`Write` > `Control` > `View`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    View = 1,
    Control = 2,
    Write = 3,
}

impl AccessLevel {
    #[must_use]
    pub const fn satisfies(self, minimum: Self) -> bool {
        (self as u8) >= (minimum as u8)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum Scope {
    All,
    Specific(String),
}

impl Scope {
    #[must_use]
    pub fn matches(&self, target_id: Option<&str>) -> bool {
        match (self, target_id) {
            (Self::All, _) => true,
            (Self::Specific(id), Some(target)) => id == target,
            (Self::Specific(_), None) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Grant,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Permission {
    pub resource: Resource,
    pub level: AccessLevel,
    pub scope: Scope,
}

impl Permission {
    #[must_use]
    pub const fn all(resource: Resource, level: AccessLevel) -> Self {
        Self {
            resource,
            level,
            scope: Scope::All,
        }
    }

    #[must_use]
    pub fn specific(resource: Resource, level: AccessLevel, id: impl Into<String>) -> Self {
        Self {
            resource,
            level,
            scope: Scope::Specific(id.into()),
        }
    }

    #[must_use]
    pub fn matches_target(&self, resource: Resource, target_id: Option<&str>) -> bool {
        self.resource == resource && self.scope.matches(target_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionOverride {
    pub effect: PermissionEffect,
    pub permission: Permission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub permissions: Vec<Permission>,
}

impl Role {
    #[must_use]
    pub fn admin() -> Self {
        use Resource::{Group, Patch, Programmer, Scene, System, Users};
        Self {
            id: "admin".to_string(),
            name: "Admin".to_string(),
            permissions: [System, Users, Patch, Programmer, Group, Scene]
                .into_iter()
                .map(|r| Permission::all(r, AccessLevel::Write))
                .collect(),
        }
    }

    #[must_use]
    pub fn operator() -> Self {
        Self {
            id: "operator".to_string(),
            name: "Operator".to_string(),
            permissions: vec![
                Permission::all(Resource::System, AccessLevel::View),
                Permission::all(Resource::Patch, AccessLevel::View),
                Permission::all(Resource::Programmer, AccessLevel::Write),
                Permission::all(Resource::Group, AccessLevel::Write),
                Permission::all(Resource::Scene, AccessLevel::Write),
            ],
        }
    }

    #[must_use]
    pub fn playback_only() -> Self {
        Self {
            id: "playback_only".to_string(),
            name: "Playback-Only".to_string(),
            permissions: vec![
                Permission::all(Resource::Scene, AccessLevel::Control),
                Permission::all(Resource::Group, AccessLevel::View),
            ],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePermissions {
    pub role_permissions: Vec<Permission>,
    pub user_overrides: Vec<PermissionOverride>,
}

impl EffectivePermissions {
    /// Checks if `required_level` is granted for `resource` and `target_id`.
    /// Explicit `Deny` overrides take precedence over `Grant` overrides and role permissions.
    #[must_use]
    pub fn is_allowed(
        &self,
        resource: Resource,
        required_level: AccessLevel,
        target_id: Option<&str>,
    ) -> bool {
        if self.user_overrides.iter().any(|ov| {
            ov.effect == PermissionEffect::Deny
                && ov.permission.matches_target(resource, target_id)
                && required_level.satisfies(ov.permission.level)
        }) {
            return false;
        }

        self.user_overrides.iter().any(|ov| {
            ov.effect == PermissionEffect::Grant
                && ov.permission.matches_target(resource, target_id)
                && ov.permission.level.satisfies(required_level)
        }) || self.role_permissions.iter().any(|perm| {
            perm.matches_target(resource, target_id) && perm.level.satisfies(required_level)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_and_override_evaluation_works() {
        let mut perms = EffectivePermissions {
            role_permissions: Role::playback_only().permissions,
            user_overrides: vec![
                PermissionOverride {
                    effect: PermissionEffect::Grant,
                    permission: Permission::specific(Resource::Scene, AccessLevel::Write, "intro"),
                },
                PermissionOverride {
                    effect: PermissionEffect::Deny,
                    permission: Permission::specific(
                        Resource::Scene,
                        AccessLevel::Control,
                        "notlicht",
                    ),
                },
            ],
        };

        assert!(perms.is_allowed(Resource::Scene, AccessLevel::View, Some("main_stage")));
        assert!(perms.is_allowed(Resource::Scene, AccessLevel::Control, Some("main_stage")));
        assert!(!perms.is_allowed(Resource::Scene, AccessLevel::Write, Some("main_stage")));

        assert!(perms.is_allowed(Resource::Scene, AccessLevel::Write, Some("intro")));

        assert!(perms.is_allowed(Resource::Scene, AccessLevel::View, Some("notlicht")));
        assert!(!perms.is_allowed(Resource::Scene, AccessLevel::Control, Some("notlicht")));
        assert!(!perms.is_allowed(Resource::Scene, AccessLevel::Write, Some("notlicht")));

        perms.user_overrides.push(PermissionOverride {
            effect: PermissionEffect::Deny,
            permission: Permission::specific(Resource::Scene, AccessLevel::View, "secret"),
        });
        assert!(!perms.is_allowed(Resource::Scene, AccessLevel::View, Some("secret")));
    }
}
