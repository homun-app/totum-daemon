//! Analisi del formato `posix_acl_xattr` di Linux.
//!
//! Il parser è isolato dal sistema operativo perché è puro: può essere provato
//! con byte sintetici su qualunque piattaforma, senza toccare ACL reali.

use super::AclPolicy;

const XATTR_VERSION: u32 = 2;
const ENTRY_SIZE: usize = 8;

const TAG_USER_OBJ: u16 = 0x01;
const TAG_USER: u16 = 0x02;
const TAG_GROUP_OBJ: u16 = 0x04;
const TAG_GROUP: u16 = 0x08;
const TAG_MASK: u16 = 0x10;
const TAG_OTHER: u16 = 0x20;

/// Diritto di aggiungere, cancellare o sostituire voci nella directory.
const PERM_WRITE: u16 = 0x02;

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    permissions: u16,
    id: u32,
}

/// Valuta l'ACL; `Err` indica un formato non interpretabile, `Ok(Some(_))` una
/// violazione con la motivazione, `Ok(None)` un ACL compatibile.
pub(crate) fn evaluate(
    bytes: &[u8],
    owner: u32,
    policy: AclPolicy,
) -> Result<Option<&'static str>, &'static str> {
    if bytes.len() < 4 {
        return Err("intestazione ACL troncata");
    }
    let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if version != XATTR_VERSION {
        return Err("versione ACL sconosciuta");
    }

    let body = &bytes[4..];
    if body.is_empty() || !body.len().is_multiple_of(ENTRY_SIZE) {
        return Err("corpo ACL malformato");
    }

    let mut entries = Vec::with_capacity(body.len() / ENTRY_SIZE);
    for chunk in body.chunks_exact(ENTRY_SIZE) {
        entries.push(Entry {
            tag: u16::from_le_bytes([chunk[0], chunk[1]]),
            permissions: u16::from_le_bytes([chunk[2], chunk[3]]),
            id: u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
        });
    }

    let mask = entries
        .iter()
        .find(|entry| entry.tag == TAG_MASK)
        .map_or(0o7, |entry| entry.permissions);

    for entry in &entries {
        let effective = match entry.tag {
            TAG_USER_OBJ | TAG_MASK => continue,
            TAG_USER => {
                if entry.id == owner {
                    continue;
                }
                entry.permissions & mask
            }
            TAG_GROUP_OBJ | TAG_GROUP => entry.permissions & mask,
            TAG_OTHER => entry.permissions,
            _ => return Err("tag ACL sconosciuto"),
        };

        let grants = match policy {
            AclPolicy::OwnerOnly => effective != 0,
            // La sola esecuzione permette di attraversare la directory, la
            // lettura di elencarla: nessuna delle due consente di sostituire la
            // voce successiva o di modificarne le protezioni. Solo la scrittura
            // modifica il contenuto della directory.
            AclPolicy::DirectoryReplacement => effective & PERM_WRITE != 0,
        };
        if grants {
            return Ok(Some(
                "voce ACL POSIX che concede accesso a un altro account",
            ));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{TAG_GROUP, TAG_GROUP_OBJ, TAG_MASK, TAG_OTHER, TAG_USER, TAG_USER_OBJ, evaluate};
    use crate::platform::AclPolicy;

    fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in entries {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&permissions.to_le_bytes());
            bytes.extend_from_slice(&id.to_le_bytes());
        }
        bytes
    }

    const OWNER: u32 = 501;

    #[test]
    fn mode_equivalent_acl_is_accepted() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_GROUP_OBJ, 0o0, u32::MAX),
            (TAG_MASK, 0o0, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);

        assert_eq!(
            evaluate(&bytes, OWNER, AclPolicy::OwnerOnly),
            Ok(None),
            "an ACL equivalent to 0700 grants nothing to others"
        );
    }

    #[test]
    fn read_access_for_other_is_a_violation_under_owner_only() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_GROUP_OBJ, 0o0, u32::MAX),
            (TAG_MASK, 0o0, u32::MAX),
            (TAG_OTHER, 0o4, u32::MAX),
        ]);

        assert!(
            evaluate(&bytes, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn named_user_write_is_a_violation_for_ancestors() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o2, 502),
            (TAG_GROUP_OBJ, 0o0, u32::MAX),
            (TAG_MASK, 0o7, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);

        assert!(
            evaluate(&bytes, OWNER, AclPolicy::DirectoryReplacement)
                .unwrap()
                .is_some()
        );
        assert!(
            evaluate(&bytes, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn mask_limits_named_user_permissions() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o6, 502),
            (TAG_GROUP_OBJ, 0o0, u32::MAX),
            (TAG_MASK, 0o0, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);

        assert_eq!(
            evaluate(&bytes, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None),
            "a zero mask makes named entries ineffective"
        );
    }

    #[test]
    fn ancestor_read_and_traverse_are_not_replacements() {
        let named = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o5, 502),
            (TAG_MASK, 0o7, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);
        assert_eq!(
            evaluate(&named, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None),
            "read and traverse on an ancestor do not allow replacing the next entry"
        );
        assert!(
            evaluate(&named, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some(),
            "the same entry is still access for another account under OwnerOnly"
        );

        let group = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_GROUP_OBJ, 0o5, u32::MAX),
            (TAG_MASK, 0o7, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);
        assert_eq!(
            evaluate(&group, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None)
        );
        assert!(
            evaluate(&group, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some()
        );

        let other = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_GROUP_OBJ, 0o0, u32::MAX),
            (TAG_MASK, 0o0, u32::MAX),
            (TAG_OTHER, 0o5, u32::MAX),
        ]);
        assert_eq!(
            evaluate(&other, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None)
        );
        assert!(
            evaluate(&other, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn ancestor_write_for_another_account_is_refused() {
        let cases = [
            (
                "named user",
                acl(&[
                    (TAG_USER_OBJ, 0o7, u32::MAX),
                    (TAG_USER, 0o2, 502),
                    (TAG_MASK, 0o7, u32::MAX),
                    (TAG_OTHER, 0o0, u32::MAX),
                ]),
            ),
            (
                "group object",
                acl(&[
                    (TAG_USER_OBJ, 0o7, u32::MAX),
                    (TAG_GROUP_OBJ, 0o2, u32::MAX),
                    (TAG_MASK, 0o7, u32::MAX),
                    (TAG_OTHER, 0o0, u32::MAX),
                ]),
            ),
            (
                "named group",
                acl(&[
                    (TAG_USER_OBJ, 0o7, u32::MAX),
                    (TAG_GROUP, 0o2, 20),
                    (TAG_MASK, 0o7, u32::MAX),
                    (TAG_OTHER, 0o0, u32::MAX),
                ]),
            ),
            (
                "other",
                acl(&[
                    (TAG_USER_OBJ, 0o7, u32::MAX),
                    (TAG_GROUP_OBJ, 0o0, u32::MAX),
                    (TAG_MASK, 0o0, u32::MAX),
                    (TAG_OTHER, 0o2, u32::MAX),
                ]),
            ),
        ];

        for (label, bytes) in cases {
            assert!(
                evaluate(&bytes, OWNER, AclPolicy::DirectoryReplacement)
                    .unwrap()
                    .is_some(),
                "{label}: write must be a replacement right on ancestors"
            );
            assert!(
                evaluate(&bytes, OWNER, AclPolicy::OwnerOnly)
                    .unwrap()
                    .is_some(),
                "{label}: write is access for another account under OwnerOnly"
            );
        }
    }

    #[test]
    fn mask_can_remove_write_from_named_and_group_entries() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o6, 502),
            (TAG_GROUP_OBJ, 0o6, u32::MAX),
            (TAG_MASK, 0o5, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);

        assert_eq!(
            evaluate(&bytes, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None),
            "the 005 mask removes write and leaves read and traverse"
        );
        assert!(
            evaluate(&bytes, OWNER, AclPolicy::OwnerOnly)
                .unwrap()
                .is_some(),
            "read and traverse still count as access for another account"
        );
    }

    #[test]
    fn named_entries_and_named_groups_combine() {
        let allowed = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o5, 502),
            (TAG_GROUP, 0o5, 20),
            (TAG_GROUP_OBJ, 0o5, u32::MAX),
            (TAG_MASK, 0o5, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);
        assert_eq!(
            evaluate(&allowed, OWNER, AclPolicy::DirectoryReplacement),
            Ok(None)
        );

        let with_write = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o5, 502),
            (TAG_GROUP, 0o3, 20),
            (TAG_GROUP_OBJ, 0o5, u32::MAX),
            (TAG_MASK, 0o7, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);
        assert!(
            evaluate(&with_write, OWNER, AclPolicy::DirectoryReplacement)
                .unwrap()
                .is_some(),
            "a named group with write and execute is a replacement right"
        );
    }

    #[test]
    fn owner_entries_do_not_count_as_other_accounts() {
        let bytes = acl(&[
            (TAG_USER_OBJ, 0o7, u32::MAX),
            (TAG_USER, 0o7, OWNER),
            (TAG_GROUP, 0o0, 20),
            (TAG_MASK, 0o7, u32::MAX),
            (TAG_OTHER, 0o0, u32::MAX),
        ]);

        assert_eq!(evaluate(&bytes, OWNER, AclPolicy::OwnerOnly), Ok(None));
    }

    #[test]
    fn malformed_and_unknown_formats_are_unverifiable() {
        assert!(evaluate(&[2, 0, 0], OWNER, AclPolicy::OwnerOnly).is_err());
        assert!(evaluate(&0u32.to_le_bytes(), OWNER, AclPolicy::OwnerOnly).is_err());
        let truncated = acl(&[(TAG_USER_OBJ, 0o7, u32::MAX)]);
        assert!(
            evaluate(
                &truncated[..truncated.len() - 1],
                OWNER,
                AclPolicy::OwnerOnly
            )
            .is_err()
        );
        let unknown = acl(&[(0x40, 0o7, u32::MAX)]);
        assert!(evaluate(&unknown, OWNER, AclPolicy::OwnerOnly).is_err());
    }
}
