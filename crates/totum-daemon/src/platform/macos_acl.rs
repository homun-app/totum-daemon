//! Lettura degli ACL estesi di macOS tramite libSystem.
//!
//! `rustix` non copre gli ACL estesi: le funzioni `acl_*` e `mbr_uuid_to_id`
//! sono dichiarate qui e usate soltanto per leggere gli ACL effettivi. Su macOS
//! `acl_get_entry` restituisce 0 quando ottiene una voce e -1 quando non ce ne
//! sono altre: è una differenza documentata rispetto a POSIX.1e.

use std::ffi::{c_int, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;
use std::ptr;

use super::{AclPolicy, PlatformError};

type Acl = *mut c_void;
type AclEntry = *mut c_void;

const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
const ACL_FIRST_ENTRY: c_int = 0;
const ACL_NEXT_ENTRY: c_int = -1;
const ACL_EXTENDED_ALLOW: c_int = 1;
const ACL_EXTENDED_DENY: c_int = 2;

const ID_TYPE_UID: c_int = 0;

const ACL_WRITE_DATA: u64 = 1 << 2;
const ACL_DELETE: u64 = 1 << 4;
const ACL_APPEND_DATA: u64 = 1 << 5;
const ACL_DELETE_CHILD: u64 = 1 << 6;
const ACL_WRITE_SECURITY: u64 = 1 << 12;
const ACL_CHANGE_OWNER: u64 = 1 << 13;

const DIRECTORY_REPLACEMENT: u64 = ACL_WRITE_DATA
    | ACL_APPEND_DATA
    | ACL_DELETE
    | ACL_DELETE_CHILD
    | ACL_WRITE_SECURITY
    | ACL_CHANGE_OWNER;

const ENOENT: i32 = 2;

unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> Acl;
    fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
    fn acl_get_tag_type(entry: AclEntry, tag: *mut c_int) -> c_int;
    fn acl_get_permset_mask_np(entry: AclEntry, mask: *mut u64) -> c_int;
    fn acl_get_qualifier(entry: AclEntry) -> *mut c_void;
    fn acl_free(object: *mut c_void) -> c_int;
    fn mbr_uuid_to_id(uuid: *const u8, id: *mut u32, id_type: *mut c_int) -> c_int;
}

pub(super) fn check(
    descriptor: BorrowedFd<'_>,
    path: &Path,
    policy: AclPolicy,
) -> Result<(), PlatformError> {
    // SAFETY: il descrittore è valido per la durata della chiamata; il valore
    // restituito è un ACL opaco liberato dal guard.
    let acl = unsafe { acl_get_fd_np(descriptor.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ENOENT) {
            return Ok(());
        }
        return Err(PlatformError::UnverifiableProtection {
            path: path.to_path_buf(),
            detail: format!("lettura degli ACL fallita: {error}"),
        });
    }
    let _guard = AclGuard(acl);

    let mut entry: AclEntry = ptr::null_mut();
    // SAFETY: `acl` è valido; `entry` riceve una vista valida finché l'ACL vive.
    let mut status = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) };
    while status == 0 {
        check_entry(entry, path, policy)?;
        // SAFETY: come sopra, l'ACL resta valido fino al guard.
        status = unsafe { acl_get_entry(acl, ACL_NEXT_ENTRY, &mut entry) };
    }
    Ok(())
}

fn check_entry(entry: AclEntry, path: &Path, policy: AclPolicy) -> Result<(), PlatformError> {
    let mut tag: c_int = 0;
    // SAFETY: `entry` proviene da un ACL valido.
    if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
        return Err(unverifiable(path, "tipo di voce ACL illeggibile"));
    }
    if tag == ACL_EXTENDED_DENY {
        return Ok(());
    }
    if tag != ACL_EXTENDED_ALLOW {
        return Err(unverifiable(path, "tipo di voce ACL sconosciuto"));
    }

    let mut mask: u64 = 0;
    // SAFETY: `entry` proviene da un ACL valido.
    if unsafe { acl_get_permset_mask_np(entry, &mut mask) } != 0 {
        return Err(unverifiable(path, "diritti della voce ACL illeggibili"));
    }

    let relevant = match policy {
        AclPolicy::OwnerOnly => mask != 0,
        AclPolicy::DirectoryReplacement => mask & DIRECTORY_REPLACEMENT != 0,
    };
    if !relevant {
        return Ok(());
    }

    // SAFETY: `entry` proviene da un ACL valido; il qualifier va liberato.
    let qualifier = unsafe { acl_get_qualifier(entry) };
    if qualifier.is_null() {
        return Err(unverifiable(path, "qualifier della voce ACL assente"));
    }
    let mut id: u32 = 0;
    let mut id_type: c_int = -1;
    // SAFETY: il qualifier è un GUID di 16 byte valido finché non viene liberato.
    let converted = unsafe { mbr_uuid_to_id(qualifier.cast::<u8>(), &mut id, &mut id_type) };
    // SAFETY: il qualifier è stato ottenuto da acl_get_qualifier.
    unsafe { acl_free(qualifier) };
    if converted != 0 {
        return Err(unverifiable(path, "qualifier ACL non risolvibile"));
    }

    if id_type == ID_TYPE_UID && id == super::euid() {
        return Ok(());
    }
    Err(PlatformError::AclViolation {
        path: path.to_path_buf(),
        detail: format!("voce ACL che concede diritti (mask {mask:#x}) a un altro utente"),
    })
}

fn unverifiable(path: &Path, detail: &str) -> PlatformError {
    PlatformError::UnverifiableProtection {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    }
}

struct AclGuard(Acl);

impl Drop for AclGuard {
    fn drop(&mut self) {
        // SAFETY: l'ACL è stato ottenuto da acl_get_fd_np e non è più usato.
        unsafe {
            acl_free(self.0);
        }
    }
}
