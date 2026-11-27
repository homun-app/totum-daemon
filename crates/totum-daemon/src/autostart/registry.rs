//! Politica delle protezioni della chiave `Run` e piano delle modifiche.
//!
//! La decisione è pura e viene provata con dati sintetici anche fuori da
//! Windows. Il backend reale legge le protezioni effettive dell'handle aperto:
//! la politica non ripara ACL, non interpreta una lettura fallita come permesso
//! e non considera sufficiente l'appartenenza a `HKCU`.

use std::path::Path;

use crate::platform::{RegistryAceKind, RegistrySecurity};

use super::{AutostartError, InstallChange, RemoveChange, Status, command_line};

pub(super) const REG_SZ: u32 = 1;

const KEY_SET_VALUE: u32 = 0x0002;
const KEY_CREATE_SUB_KEY: u32 = 0x0004;
const KEY_CREATE_LINK: u32 = 0x0020;
const DELETE: u32 = 0x0001_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

const SYSTEM: &str = "S-1-5-18";
const ADMINISTRATORS: &str = "S-1-5-32-544";

/// Diritti che permettono di modificare il valore o le protezioni della chiave.
const KEY_WRITE: u32 = KEY_SET_VALUE
    | KEY_CREATE_SUB_KEY
    | KEY_CREATE_LINK
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL
    | MAXIMUM_ALLOWED;

/// Diritti che permettono di sostituire la chiave o le protezioni della voce
/// successiva. La creazione di una chiave affine non sostituisce `Run`.
const ANCESTOR_REPLACE: u32 =
    DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL | MAXIMUM_ALLOWED;

/// Ruolo della chiave nella catena verificata.
#[derive(Clone, Copy, Debug)]
pub(super) enum Role {
    /// La chiave `Run` stessa.
    Key,
    /// Un suo antenato.
    Ancestor,
}

impl Role {
    fn mask(self) -> u32 {
        match self {
            Role::Key => KEY_WRITE,
            Role::Ancestor => ANCESTOR_REPLACE,
        }
    }
}

/// Violazione della politica delle protezioni.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PolicyError {
    pub detail: String,
}

impl PolicyError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Valore registrato nella chiave `Run`, senza interpretare contenuti estranei.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Value {
    Absent,
    Present { kind: u32, text: Option<String> },
}

/// Stato osservato prima di una modifica.
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub key: Option<RegistrySecurity>,
    pub ancestors: Vec<RegistrySecurity>,
    pub value: Value,
}

/// Azione decisa per una registrazione.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InstallPlan {
    Create,
    Update,
    Unchanged,
}

/// Azione decisa per una rimozione.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemovePlan {
    Remove,
    Absent,
}

/// Errore di piano: protezione non ammessa o oggetto estraneo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PlanError {
    Unsafe(String),
    Incompatible(String),
}

impl PlanError {
    pub(super) fn into_autostart(self, path: &Path) -> AutostartError {
        match self {
            PlanError::Unsafe(detail) => AutostartError::UnsafeObject {
                path: path.to_path_buf(),
                detail,
            },
            PlanError::Incompatible(detail) => AutostartError::IncompatibleObject {
                path: path.to_path_buf(),
                detail,
            },
        }
    }
}

/// Backend minimo usato dal pianificatore e sostituibile nei test.
pub(super) trait Backend {
    fn snapshot(&mut self) -> Result<Snapshot, AutostartError>;
    fn write_value(&mut self, command: &str, expected: &Snapshot) -> Result<(), AutostartError>;
    fn delete_value(&mut self, expected: &Snapshot) -> Result<(), AutostartError>;
}

/// Registra il valore solo dopo che il piano ha accettato protezioni e valore.
pub(super) fn install(
    backend: &mut impl Backend,
    command: &str,
    user: &str,
    path: &Path,
) -> Result<InstallChange, AutostartError> {
    let snapshot = backend.snapshot()?;
    let plan =
        plan_install(&snapshot, command, user).map_err(|error| error.into_autostart(path))?;
    match plan {
        InstallPlan::Unchanged => Ok(InstallChange::Unchanged),
        InstallPlan::Create => {
            backend.write_value(command, &snapshot)?;
            Ok(InstallChange::Created)
        }
        InstallPlan::Update => {
            backend.write_value(command, &snapshot)?;
            Ok(InstallChange::Updated)
        }
    }
}

/// Rimuove il valore solo dopo che il piano ha accettato protezioni e valore.
pub(super) fn remove(
    backend: &mut impl Backend,
    user: &str,
    path: &Path,
) -> Result<RemoveChange, AutostartError> {
    let snapshot = backend.snapshot()?;
    let plan = plan_remove(&snapshot, user).map_err(|error| error.into_autostart(path))?;
    match plan {
        RemovePlan::Absent => Ok(RemoveChange::AlreadyAbsent),
        RemovePlan::Remove => {
            backend.delete_value(&snapshot)?;
            Ok(RemoveChange::Removed)
        }
    }
}

/// Classifica lo stato senza modificare nulla; una protezione non ammessa
/// della chiave o di un antenato è riportata come oggetto incompatibile.
pub(super) fn status(snapshot: &Snapshot, user: &str) -> Status {
    // Una chiave assente non è una registrazione: come `plan_remove`, non
    // valuta le protezioni di un oggetto che non esiste.
    if snapshot.key.is_none() {
        return Status::Absent;
    }
    if let Err(error) = check_protections(snapshot, user) {
        return Status::Incompatible {
            detail: error.detail,
        };
    }
    match &snapshot.value {
        Value::Absent => Status::Absent,
        Value::Present {
            kind: REG_SZ,
            text: Some(text),
        } => match command_line::parse_owned(text) {
            Some(program) => Status::Registered { program },
            None => Status::Incompatible {
                detail: "valore Run non riconosciuto come registrazione di Totum".to_owned(),
            },
        },
        Value::Present { kind, text: None } => Status::Incompatible {
            detail: format!("valore Run non decodificabile (tipo {kind})"),
        },
        Value::Present { kind, .. } => Status::Incompatible {
            detail: format!("tipo di valore Run inatteso: {kind}"),
        },
    }
}

/// Decide la registrazione dopo aver verificato antenati, chiave e valore.
pub(super) fn plan_install(
    snapshot: &Snapshot,
    command: &str,
    user: &str,
) -> Result<InstallPlan, PlanError> {
    check_protections(snapshot, user).map_err(|error| PlanError::Unsafe(error.detail))?;
    match &snapshot.value {
        Value::Absent => Ok(InstallPlan::Create),
        Value::Present {
            kind: REG_SZ,
            text: Some(text),
        } if text == command => Ok(InstallPlan::Unchanged),
        Value::Present {
            kind: REG_SZ,
            text: Some(text),
        } if command_line::parse_owned(text).is_some() => Ok(InstallPlan::Update),
        Value::Present {
            kind: REG_SZ,
            text: Some(_),
        } => Err(PlanError::Incompatible(
            "valore Run estraneo o con argomenti".to_owned(),
        )),
        Value::Present {
            kind: REG_SZ,
            text: None,
        } => Err(PlanError::Incompatible(
            "valore Run REG_SZ non decodificabile".to_owned(),
        )),
        Value::Present { kind, .. } => Err(PlanError::Incompatible(format!(
            "tipo di valore Run inatteso: {kind}"
        ))),
    }
}

/// Decide la rimozione dopo aver verificato antenati, chiave e valore.
pub(super) fn plan_remove(snapshot: &Snapshot, user: &str) -> Result<RemovePlan, PlanError> {
    if snapshot.key.is_none() {
        return Ok(RemovePlan::Absent);
    }
    check_protections(snapshot, user).map_err(|error| PlanError::Unsafe(error.detail))?;
    match &snapshot.value {
        Value::Absent => Ok(RemovePlan::Absent),
        Value::Present {
            kind: REG_SZ,
            text: Some(text),
        } if command_line::parse_owned(text).is_some() => Ok(RemovePlan::Remove),
        Value::Present {
            kind: REG_SZ,
            text: Some(_),
        } => Err(PlanError::Incompatible(
            "valore Run estraneo o con argomenti".to_owned(),
        )),
        Value::Present {
            kind: REG_SZ,
            text: None,
        } => Err(PlanError::Incompatible(
            "valore Run REG_SZ non decodificabile".to_owned(),
        )),
        Value::Present { kind, .. } => Err(PlanError::Incompatible(format!(
            "tipo di valore Run inatteso: {kind}"
        ))),
    }
}

fn check_protections(snapshot: &Snapshot, user: &str) -> Result<(), PolicyError> {
    for ancestor in &snapshot.ancestors {
        check_security(ancestor, Role::Ancestor, user)?;
    }
    if let Some(key) = &snapshot.key {
        check_security(key, Role::Key, user)?;
    }
    Ok(())
}

/// Verifica proprietario, DACL e titolari dei diritti di modifica.
pub(super) fn check_security(
    security: &RegistrySecurity,
    role: Role,
    user: &str,
) -> Result<(), PolicyError> {
    let owner = security
        .owner
        .as_deref()
        .ok_or_else(|| PolicyError::new("proprietario non leggibile"))?;
    if !is_trusted_owner(owner, user) {
        return Err(PolicyError::new(format!(
            "proprietario inatteso {owner}: atteso l'account corrente, SYSTEM o gli amministratori"
        )));
    }
    if !security.dacl_present {
        return Err(PolicyError::new(
            "assenza di DACL: accesso completo a tutti gli account",
        ));
    }
    for entry in &security.entries {
        if entry.inherit_only {
            continue;
        }
        match entry.kind {
            RegistryAceKind::Denied | RegistryAceKind::Audit | RegistryAceKind::Alarm => continue,
            RegistryAceKind::Other => {
                return Err(PolicyError::new(
                    "tipo di ACE non interpretabile nella DACL della chiave Run",
                ));
            }
            RegistryAceKind::Allowed => {}
        }
        if entry.mask & role.mask() == 0 {
            continue;
        }
        let trustee = entry
            .trustee
            .as_deref()
            .ok_or_else(|| PolicyError::new("SID di una voce ACL non convertibile"))?;
        if trustee == user || is_trusted_writer(trustee) {
            continue;
        }
        return Err(PolicyError::new(match role {
            Role::Key => format!(
                "l'account {trustee} può modificare il valore Run o le protezioni della chiave"
            ),
            Role::Ancestor => format!(
                "l'account {trustee} può sostituire la chiave Run o modificarne le protezioni"
            ),
        }));
    }
    Ok(())
}

fn is_trusted_owner(sid: &str, user: &str) -> bool {
    sid == user || is_trusted_writer(sid)
}

fn is_trusted_writer(sid: &str) -> bool {
    sid == SYSTEM || sid == ADMINISTRATORS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{RegistryAce, RegistrySecurity};
    use std::path::PathBuf;

    const USER: &str = "S-1-5-21-1111-2222-3333-1001";
    const OTHER: &str = "S-1-5-21-1111-2222-3333-1002";
    const USERS: &str = "S-1-5-32-545";

    fn ace(kind: RegistryAceKind, mask: u32, trustee: &str) -> RegistryAce {
        RegistryAce {
            kind,
            inherit_only: false,
            mask,
            trustee: Some(trustee.to_owned()),
        }
    }

    fn allowed(entries: Vec<RegistryAce>) -> RegistrySecurity {
        RegistrySecurity {
            owner: Some(USER.to_owned()),
            dacl_present: true,
            protected: true,
            entries,
        }
    }

    fn snapshot(key: Option<RegistrySecurity>, value: Value) -> Snapshot {
        Snapshot {
            key,
            ancestors: vec![allowed(vec![
                ace(RegistryAceKind::Allowed, KEY_READ, USER),
                ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, SYSTEM),
                ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, ADMINISTRATORS),
                ace(RegistryAceKind::Allowed, KEY_READ, USERS),
            ])],
            value,
        }
    }

    const KEY_READ: u32 = 0x0002_0019;
    const KEY_ALL_ACCESS: u32 = 0x000F_003F;

    #[derive(Default)]
    struct Fake {
        snapshot: Option<Snapshot>,
        writes: Vec<String>,
        deletes: usize,
    }

    impl Backend for Fake {
        fn snapshot(&mut self) -> Result<Snapshot, AutostartError> {
            Ok(self.snapshot.clone().expect("snapshot di prova"))
        }

        fn write_value(
            &mut self,
            command: &str,
            _expected: &Snapshot,
        ) -> Result<(), AutostartError> {
            self.writes.push(command.to_owned());
            Ok(())
        }

        fn delete_value(&mut self, _expected: &Snapshot) -> Result<(), AutostartError> {
            self.deletes += 1;
            Ok(())
        }
    }

    fn path() -> PathBuf {
        PathBuf::from("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run")
    }

    #[test]
    fn allowed_protections_create_the_value_once() {
        let mut fake = Fake {
            snapshot: Some(snapshot(
                Some(allowed(vec![ace(
                    RegistryAceKind::Allowed,
                    KEY_ALL_ACCESS,
                    USER,
                )])),
                Value::Absent,
            )),
            ..Fake::default()
        };
        let change = install(&mut fake, "C:\\Totum\\totum-daemon.exe", USER, &path()).unwrap();
        assert_eq!(change, InstallChange::Created);
        assert_eq!(fake.writes.len(), 1);
        assert_eq!(fake.deletes, 0);
    }

    #[test]
    fn a_write_grant_to_an_untrusted_account_is_refused_without_modification() {
        let mut fake = Fake {
            snapshot: Some(snapshot(
                Some(allowed(vec![
                    ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
                    ace(RegistryAceKind::Allowed, KEY_SET_VALUE, OTHER),
                ])),
                Value::Absent,
            )),
            ..Fake::default()
        };
        let error = install(&mut fake, "C:\\Totum\\totum-daemon.exe", USER, &path()).unwrap_err();
        assert!(matches!(error, AutostartError::UnsafeObject { .. }));
        assert!(fake.writes.is_empty());
        assert_eq!(fake.deletes, 0);
    }

    #[test]
    fn an_ancestor_that_another_account_can_replace_is_refused() {
        let mut snapshot = snapshot(None, Value::Absent);
        snapshot.ancestors = vec![allowed(vec![
            ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
            ace(RegistryAceKind::Allowed, DELETE, OTHER),
        ])];
        let mut fake = Fake {
            snapshot: Some(snapshot),
            ..Fake::default()
        };
        let error = install(&mut fake, "C:\\Totum\\totum-daemon.exe", USER, &path()).unwrap_err();
        assert!(matches!(error, AutostartError::UnsafeObject { .. }));
        assert!(fake.writes.is_empty());
    }

    #[test]
    fn unreadable_protections_are_refused_without_modification() {
        let mut fake = Fake {
            snapshot: Some(snapshot(
                Some(RegistrySecurity {
                    owner: None,
                    dacl_present: false,
                    protected: false,
                    entries: Vec::new(),
                }),
                Value::Absent,
            )),
            ..Fake::default()
        };
        let error = install(&mut fake, "C:\\Totum\\totum-daemon.exe", USER, &path()).unwrap_err();
        assert!(matches!(error, AutostartError::UnsafeObject { .. }));
        assert!(fake.writes.is_empty());
    }

    #[test]
    fn an_untrusted_account_cannot_remove_the_value() {
        let mut fake = Fake {
            snapshot: Some(snapshot(
                Some(allowed(vec![
                    ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
                    ace(RegistryAceKind::Allowed, DELETE, OTHER),
                ])),
                Value::Present {
                    kind: REG_SZ,
                    text: Some("C:\\Totum\\totum-daemon.exe".to_owned()),
                },
            )),
            ..Fake::default()
        };
        let error = remove(&mut fake, USER, &path()).unwrap_err();
        assert!(matches!(error, AutostartError::UnsafeObject { .. }));
        assert_eq!(fake.deletes, 0);
    }

    #[test]
    fn denied_audit_alarm_and_inherit_only_entries_do_not_grant_write() {
        let security = allowed(vec![
            ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
            ace(RegistryAceKind::Denied, KEY_SET_VALUE, OTHER),
            ace(RegistryAceKind::Audit, KEY_SET_VALUE, OTHER),
            ace(RegistryAceKind::Alarm, KEY_SET_VALUE, OTHER),
            RegistryAce {
                kind: RegistryAceKind::Allowed,
                inherit_only: true,
                mask: DELETE,
                trustee: Some(OTHER.to_owned()),
            },
        ]);
        check_security(&security, Role::Key, USER).expect("le voci non effettive sono ignorate");

        let unknown = allowed(vec![
            ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
            RegistryAce {
                kind: RegistryAceKind::Other,
                inherit_only: false,
                mask: 0x1,
                trustee: None,
            },
        ]);
        assert!(check_security(&unknown, Role::Key, USER).is_err());
    }

    #[test]
    fn status_does_not_modify_and_reports_unsafe_protections() {
        let snapshot = snapshot(
            Some(allowed(vec![
                ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
                ace(RegistryAceKind::Allowed, KEY_SET_VALUE, OTHER),
            ])),
            Value::Absent,
        );
        assert!(matches!(
            status(&snapshot, USER),
            Status::Incompatible { .. }
        ));
    }

    const CANONICAL: &str = "C:\\Totum\\totum-daemon.exe";

    fn canonical_value() -> Value {
        Value::Present {
            kind: REG_SZ,
            text: Some(CANONICAL.to_owned()),
        }
    }

    fn safe_key() -> RegistrySecurity {
        allowed(vec![ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER)])
    }

    fn unsafe_ancestors() -> Vec<RegistrySecurity> {
        vec![allowed(vec![
            ace(RegistryAceKind::Allowed, KEY_ALL_ACCESS, USER),
            ace(RegistryAceKind::Allowed, WRITE_DAC, OTHER),
        ])]
    }

    #[test]
    fn status_refuses_an_unsafe_ancestor_for_a_canonical_registration() {
        let mut snapshot = snapshot(Some(safe_key()), canonical_value());
        snapshot.ancestors = unsafe_ancestors();
        let Status::Incompatible { detail } = status(&snapshot, USER) else {
            panic!("una registrazione sotto un antenato non sicuro non è Registered");
        };
        assert!(detail.contains(OTHER), "{detail}");
        assert!(detail.contains("sostituire"), "{detail}");
    }

    #[test]
    fn status_accepts_a_safe_chain_with_a_canonical_registration() {
        let snapshot = snapshot(Some(safe_key()), canonical_value());
        let Status::Registered { program } = status(&snapshot, USER) else {
            panic!("una catena sicura con un valore canonico è Registered");
        };
        assert_eq!(program, PathBuf::from(CANONICAL));
    }

    #[test]
    fn status_reports_absence_without_inspecting_a_missing_key() {
        let mut snapshot = snapshot(None, Value::Absent);
        snapshot.ancestors = unsafe_ancestors();
        assert!(matches!(status(&snapshot, USER), Status::Absent));
    }

    #[test]
    fn status_reports_absence_for_a_present_key_without_a_value_on_a_safe_chain() {
        let snapshot = snapshot(Some(safe_key()), Value::Absent);
        assert!(matches!(status(&snapshot, USER), Status::Absent));
    }

    #[test]
    fn foreign_and_unexpected_values_are_not_overwritten() {
        for value in [
            Value::Present {
                kind: REG_SZ,
                text: Some("C:\\Other\\other.exe --flag".to_owned()),
            },
            Value::Present {
                kind: 4,
                text: Some("foreign".to_owned()),
            },
        ] {
            let mut fake = Fake {
                snapshot: Some(snapshot(
                    Some(allowed(vec![ace(
                        RegistryAceKind::Allowed,
                        KEY_ALL_ACCESS,
                        USER,
                    )])),
                    value,
                )),
                ..Fake::default()
            };
            let error =
                install(&mut fake, "C:\\Totum\\totum-daemon.exe", USER, &path()).unwrap_err();
            assert!(matches!(error, AutostartError::IncompatibleObject { .. }));
            assert!(fake.writes.is_empty());
        }
    }
}
