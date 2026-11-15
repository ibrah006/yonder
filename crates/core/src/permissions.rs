//! What a paired controller is allowed to do on a machine.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One thing a controller may be allowed to do.
///
/// On the wire variants are encoded by position: only ever append new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Run jobs as the machine's build user.
    RunJobs,
    /// Open terminal sessions.
    Terminal,
    /// Run things with administrator rights. Off unless the machine's owner turns it on.
    Admin,
    /// See and remove what this controller left on the machine (credentials, repo copies).
    Footprint,
    /// Revoke other controllers. Used for "lost laptop" recovery from another device.
    ManageMachine,
}

impl Permission {
    pub const ALL: [Permission; 5] = [
        Permission::RunJobs,
        Permission::Terminal,
        Permission::Admin,
        Permission::Footprint,
        Permission::ManageMachine,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Permission::RunJobs => "run_jobs",
            Permission::Terminal => "terminal",
            Permission::Admin => "admin",
            Permission::Footprint => "footprint",
            Permission::ManageMachine => "manage_machine",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of permissions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Permissions(BTreeSet<Permission>);

impl Permissions {
    /// What a newly paired controller gets: everything except admin rights.
    pub fn for_new_controller() -> Self {
        Permissions(
            [
                Permission::RunJobs,
                Permission::Terminal,
                Permission::Footprint,
                Permission::ManageMachine,
            ]
            .into(),
        )
    }

    pub fn contains(&self, permission: Permission) -> bool {
        self.0.contains(&permission)
    }

    pub fn insert(&mut self, permission: Permission) {
        self.0.insert(permission);
    }

    pub fn remove(&mut self, permission: Permission) {
        self.0.remove(&permission);
    }

    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.0.iter().copied()
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        let names: Vec<_> = self.iter().map(Permission::name).collect();
        f.write_str(&names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_controllers_do_not_get_admin() {
        let p = Permissions::for_new_controller();
        assert!(!p.contains(Permission::Admin));
        assert!(p.contains(Permission::RunJobs));
        assert!(p.contains(Permission::Footprint));
    }

    #[test]
    fn names_round_trip() {
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.name()), Some(p));
        }
    }

    #[test]
    fn json_uses_readable_names() {
        let mut p = Permissions::default();
        p.insert(Permission::Terminal);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"["terminal"]"#);
    }
}
