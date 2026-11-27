//! Lettura degli ACL POSIX effettivi su Linux.
//!
//! L'ACL è conservata nell'attributo esteso `system.posix_acl_access` nel
//! formato `posix_acl_xattr`, analizzato da [`super::posix_acl`]. Un file system
//! senza ACL restituisce `ENODATA` o `ENOTSUP`, che non è un errore: senza ACL
//! valgono i bit di modo, già verificati dal chiamante. Un ACL presente ma
//! illeggibile o malformato è una protezione non verificabile e produce un
//! errore.

use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use rustix::fs as rfs;
use rustix::io::Errno;

use super::posix_acl;
use super::{AclPolicy, PlatformError};

const XATTR_NAME: &str = "system.posix_acl_access";

pub(super) fn check(
    descriptor: BorrowedFd<'_>,
    path: &Path,
    policy: AclPolicy,
) -> Result<(), PlatformError> {
    let size = match rfs::fgetxattr_query(descriptor, XATTR_NAME) {
        Ok(size) => size,
        Err(errno) if errno == Errno::NODATA || errno == Errno::NOTSUP => return Ok(()),
        Err(errno) => {
            return Err(PlatformError::UnverifiableProtection {
                path: path.to_path_buf(),
                detail: format!("lettura dell'attributo ACL fallita: {errno}"),
            });
        }
    };
    if size == 0 {
        return Ok(());
    }

    let mut buffer = vec![0u8; size];
    let read = rfs::fgetxattr(descriptor.as_fd(), XATTR_NAME, &mut buffer).map_err(|errno| {
        PlatformError::UnverifiableProtection {
            path: path.to_path_buf(),
            detail: format!("lettura dell'attributo ACL fallita: {errno}"),
        }
    })?;
    buffer.truncate(read);

    match posix_acl::evaluate(&buffer, super::euid(), policy) {
        Ok(Some(detail)) => Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: detail.to_owned(),
        }),
        Ok(None) => Ok(()),
        Err(detail) => Err(PlatformError::UnverifiableProtection {
            path: path.to_path_buf(),
            detail: detail.to_owned(),
        }),
    }
}
