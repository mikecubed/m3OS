//! Credential transition logic for setuid/setgid enforcement (Phase 48).

/// Process credential state for privilege checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
}

/// Error returned when a credential transition is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionDenied;

impl Credentials {
    /// Apply setuid rules:
    /// - If euid == 0 (root): set both uid and euid to `new_uid`
    /// - If euid != 0: only allow setting euid back to the real uid
    /// - Otherwise: return PermissionDenied
    pub fn set_uid(&mut self, new_uid: u32) -> Result<(), PermissionDenied> {
        if self.euid == 0 {
            self.uid = new_uid;
            self.euid = new_uid;
            Ok(())
        } else if new_uid == self.uid {
            self.euid = new_uid;
            Ok(())
        } else {
            Err(PermissionDenied)
        }
    }

    /// Apply setgid rules (mirrors set_uid for gid/egid).
    /// Privilege check is based on euid (not egid), matching Linux behavior.
    pub fn set_gid(&mut self, new_gid: u32) -> Result<(), PermissionDenied> {
        if self.euid == 0 {
            self.gid = new_gid;
            self.egid = new_gid;
            Ok(())
        } else if new_gid == self.gid {
            self.egid = new_gid;
            Ok(())
        } else {
            Err(PermissionDenied)
        }
    }

    /// Apply setreuid rules:
    /// - ruid change: only if euid==0 or new_ruid matches current real or effective uid
    /// - euid change: only if euid==0 or new_euid matches current real, effective, or saved uid
    ///
    /// Pass -1 (as i32) for either argument to leave that field unchanged.
    pub fn set_reuid(&mut self, new_ruid: i32, new_euid: i32) -> Result<(), PermissionDenied> {
        if new_ruid != -1 {
            let r = new_ruid as u32;
            if self.euid != 0 && r != self.uid && r != self.euid {
                return Err(PermissionDenied);
            }
            self.uid = r;
        }
        if new_euid != -1 {
            let e = new_euid as u32;
            if self.euid != 0 && e != self.uid && e != self.euid {
                return Err(PermissionDenied);
            }
            self.euid = e;
        }
        Ok(())
    }

    /// Apply setregid rules (mirrors set_reuid for gid/egid).
    /// Privilege check is based on euid (not egid), matching Linux behavior.
    pub fn set_regid(&mut self, new_rgid: i32, new_egid: i32) -> Result<(), PermissionDenied> {
        if new_rgid != -1 {
            let r = new_rgid as u32;
            if self.euid != 0 && r != self.gid && r != self.egid {
                return Err(PermissionDenied);
            }
            self.gid = r;
        }
        if new_egid != -1 {
            let e = new_egid as u32;
            if self.euid != 0 && e != self.gid && e != self.egid {
                return Err(PermissionDenied);
            }
            self.egid = e;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setuid_root_can_set_any() {
        let mut cred = Credentials {
            uid: 0,
            gid: 0,
            euid: 0,
            egid: 0,
        };
        assert!(cred.set_uid(1000).is_ok());
        assert_eq!(cred.uid, 1000);
        assert_eq!(cred.euid, 1000);
    }

    #[test]
    fn test_setuid_nonroot_denied() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 1000,
            euid: 1000,
            egid: 1000,
        };
        assert_eq!(cred.set_uid(0), Err(PermissionDenied));
        // uid and euid should be unchanged
        assert_eq!(cred.uid, 1000);
        assert_eq!(cred.euid, 1000);
    }

    #[test]
    fn test_setuid_nonroot_can_restore_real_uid() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 1000,
            euid: 500,
            egid: 1000,
        };
        // Can set back to real uid
        assert!(cred.set_uid(1000).is_ok());
        assert_eq!(cred.euid, 1000);
    }

    #[test]
    fn test_setgid_nonroot_denied() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 1000,
            euid: 1000,
            egid: 1000,
        };
        assert_eq!(cred.set_gid(0), Err(PermissionDenied));
        assert_eq!(cred.gid, 1000);
        assert_eq!(cred.egid, 1000);
    }

    #[test]
    fn test_setgid_root_can_set_any() {
        let mut cred = Credentials {
            uid: 0,
            gid: 0,
            euid: 0,
            egid: 0,
        };
        assert!(cred.set_gid(500).is_ok());
        assert_eq!(cred.gid, 500);
        assert_eq!(cred.egid, 500);
    }

    #[test]
    fn test_setreuid_nonroot_can_swap_real_effective() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 0,
            euid: 1000,
            egid: 0,
        };
        // Setting ruid to own uid and euid to own uid -- both should succeed
        assert!(cred.set_reuid(1000, 1000).is_ok());
    }

    #[test]
    fn test_setreuid_nonroot_cannot_escalate() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 0,
            euid: 1000,
            egid: 0,
        };
        assert_eq!(cred.set_reuid(-1, 0), Err(PermissionDenied));
    }

    #[test]
    fn test_setregid_nonroot_cannot_escalate() {
        let mut cred = Credentials {
            uid: 1000,
            gid: 1000,
            euid: 1000,
            egid: 1000,
        };
        assert_eq!(cred.set_regid(-1, 0), Err(PermissionDenied));
    }
}
