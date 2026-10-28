//! Who may use the control socket: root and members of the cellglide group.

use std::sync::Arc;

type GroupsOf = Arc<dyn Fn(u32) -> Vec<u32> + Send + Sync>;

#[derive(Clone)]
pub struct Policy {
    group_gid: Option<u32>,
    groups_of: GroupsOf,
    extra_uid: Option<u32>,
    /// The uid the service itself runs as.
    self_uid: Option<u32>,
}

impl Policy {
    pub fn for_test(
        group_gid: Option<u32>,
        groups_of: impl Fn(u32) -> Vec<u32> + Send + Sync + 'static,
    ) -> Policy {
        Policy {
            group_gid,
            groups_of: Arc::new(groups_of),
            extra_uid: None,
            self_uid: None,
        }
    }

    /// Also allow this uid (tests run the server as an ordinary user).
    #[cfg(test)]
    pub fn with_extra_uid(mut self, uid: u32) -> Policy {
        self.extra_uid = Some(uid);
        self
    }

    #[cfg(test)]
    pub fn with_self_uid(mut self, uid: u32) -> Policy {
        self.self_uid = Some(uid);
        self
    }

    pub fn allows(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || self.extra_uid == Some(uid) || self.self_uid == Some(uid) {
            return true;
        }
        self.group_gid
            .is_some_and(|group| gid == group || (self.groups_of)(uid).contains(&group))
    }

    #[cfg(target_os = "linux")]
    pub fn system(group: &str) -> Policy {
        use nix::unistd::{Group, Uid, User, getgrouplist};
        use std::ffi::CString;
        let gid = Group::from_name(group)
            .ok()
            .flatten()
            .map(|g| g.gid.as_raw());
        let mut policy = Policy::for_test(gid, |uid| {
            let Ok(Some(user)) = User::from_uid(Uid::from_raw(uid)) else {
                return Vec::new();
            };
            let Ok(name) = CString::new(user.name) else {
                return Vec::new();
            };
            getgrouplist(&name, user.gid)
                .map(|groups| groups.into_iter().map(|g| g.as_raw()).collect())
                .unwrap_or_default()
        });
        policy.self_uid = Some(nix::unistd::getuid().as_raw());
        policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_group_members_only() {
        let policy = Policy::for_test(Some(500), |uid| {
            if uid == 1001 { vec![20, 500] } else { vec![20] }
        });
        assert!(policy.allows(0, 0));
        assert!(policy.allows(1000, 500), "primary group");
        assert!(policy.allows(1001, 20), "supplementary group");
        assert!(!policy.allows(1002, 20));
        let no_group = Policy::for_test(None, |_| vec![500]);
        assert!(!no_group.allows(1001, 500));
        assert!(no_group.allows(0, 0));
        let own = Policy::for_test(None, |_| vec![]).with_self_uid(990);
        assert!(own.allows(990, 990), "the service's own uid");
        assert!(!own.allows(991, 991));
    }
}
