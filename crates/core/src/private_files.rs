//! File modes for anything that must not be world-readable.
//!
//! Nothing in this project used to set a mode, so every private key, every group
//! file and every `.tmp` sibling landed at whatever the process umask allowed -
//! `0644` on a typical developer machine. The fix is a mode at *creation* rather
//! than a `chmod` afterwards: a key that exists world-readable for a millisecond
//! is a key that was world-readable.
//!
//! Two shapes cover every call site:
//!
//! - [`create`] replaces `File::create` where this project writes the bytes itself.
//! - [`reserve`] is for the files `openssl` writes. The subprocess opens its
//!   `-out` path with `fopen(.., "w")`, which truncates an existing file and
//!   leaves its mode alone, so creating the file first at `0600` is what decides
//!   the mode of what openssl then writes into it.
//!
//! [`dir`] is the same idea for the certificate directory, which is `0700`
//! because the names in it are as much of a list of authorized clients as the
//! certificates are.
//!
//! **Windows has no equivalent here.** `PermissionsExt` is Unix-only and this
//! module's functions are plain creation calls there, so the files land at
//! whatever the default ACL grants. That gap is stated in `docs/security.md`
//! rather than silently skipped.

use std::fs::File;
use std::io;
use std::path::Path;

/// Mode for a file that only the owning process's user may read.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;
/// Mode for a directory whose entries are themselves sensitive.
#[cfg(unix)]
pub const DIR_MODE: u32 = 0o700;

/// `File::create`, with the file owner-only from the moment it exists.
///
/// The mode is applied on creation *and* to a file that was already there:
/// `File::create` truncates an existing file rather than replacing it, so a
/// group file written before this module existed would otherwise keep its
/// original `0644` forever.
pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(FILE_MODE)
            .open(path)?;
        restrict(path)?;
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        File::create(path)
    }
}

/// `std::fs::write`, with the same mode guarantee as [`create`].
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    let mut file = create(path)?;
    file.write_all(contents.as_ref())
}

/// Creates `path` empty and owner-only, so that a subprocess writing to it
/// inherits the mode instead of choosing one.
///
/// Returns the path back for use in an argument list, which keeps the call at
/// the openssl invocation it protects rather than several lines above it.
pub fn reserve(path: impl AsRef<Path>) -> io::Result<()> {
    create(path).map(|_| ())
}

/// `create_dir_all`, with the directory owner-only.
pub fn dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// Tightens a file that already exists. The escape hatch for a path this
/// project does not open itself and cannot reserve in advance.
pub fn restrict(path: impl AsRef<Path>) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path.as_ref(), std::fs::Permissions::from_mode(FILE_MODE))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The mode bits of `path`, or `None` off Unix. Tests assert against this
/// rather than reaching for `PermissionsExt` behind their own `cfg`.
pub fn mode_of(path: impl AsRef<Path>) -> io::Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(Some(std::fs::metadata(path.as_ref())?.permissions().mode() & 0o777))
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::metadata(path.as_ref())?;
        Ok(None)
    }
}

/// The async twin of [`create`], for the streamed-transfer path, which writes a
/// directory record straight off the socket on a tokio task.
pub async fn create_async(path: impl AsRef<Path>) -> io::Result<tokio::fs::File> {
    let path = path.as_ref();
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let file = options.open(path).await?;
    restrict(path)?;
    Ok(file)
}
