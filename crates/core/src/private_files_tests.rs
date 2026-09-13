//! Modes are asserted, not assumed.
//!
//! Every test here is Unix-gated at the assertion rather than at the test, so
//! the creation paths still run on Windows and a panic there is still a
//! failure - only the mode claim is skipped, which is the only part Windows
//! cannot make.

use crate::private_files;
use crate::test_support::TempDir;
use std::io::Write;

/// `None` off Unix, which every assertion below treats as "nothing to check".
fn mode(path: &std::path::Path) -> Option<u32> {
    private_files::mode_of(path).unwrap()
}

#[test]
fn test_a_created_file_is_owner_only() {
    let dir = TempDir::new("modes_create");
    let path = std::path::Path::new(dir.path()).join("secret");
    let mut file = private_files::create(&path).unwrap();
    file.write_all(b"contents").unwrap();
    drop(file);

    assert_eq!(mode(&path), Some(0o600));
    assert_eq!(std::fs::read(&path).unwrap(), b"contents");
}

#[cfg(unix)]
#[test]
fn test_a_file_from_before_this_existed_is_tightened_on_rewrite() {
    // A database written by an earlier build has group files at the umask.
    // `File::create` truncates rather than replaces, so a mode applied only at
    // creation would never reach them; the rewrite is what fixes them. The test
    // is Unix-only because starting at `0644` is itself a Unix act.
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("modes_upgrade");
    let path = std::path::Path::new(dir.path()).join("group");
    std::fs::write(&path, b"old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(mode(&path), Some(0o644));

    let mut file = private_files::create(&path).unwrap();
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(mode(&path), Some(0o600));
}

#[test]
fn test_a_reserved_file_keeps_its_mode_when_something_else_writes_it() {
    // The assumption the openssl call sites rest on: a subprocess opening the
    // path with `fopen(.., "w")` truncates the file rather than creating one,
    // and a truncation does not touch the mode. If this ever stopped being
    // true, every generated private key would quietly go back to the umask.
    let dir = TempDir::new("modes_reserve");
    let path = std::path::Path::new(dir.path()).join("ca.key");
    private_files::reserve(&path).unwrap();
    assert_eq!(mode(&path), Some(0o600));

    let mut written_by_someone_else = std::fs::File::create(&path).unwrap();
    written_by_someone_else
        .write_all(b"-----BEGIN PRIVATE KEY-----")
        .unwrap();
    drop(written_by_someone_else);

    assert_eq!(mode(&path), Some(0o600));
}

#[test]
fn test_a_created_directory_is_owner_only() {
    let dir = TempDir::new("modes_dir");
    let path = std::path::Path::new(dir.path()).join("certs");
    private_files::dir(&path).unwrap();
    assert_eq!(mode(&path), Some(0o700));
}

/// Every regular file under `root`, so an assertion can cover a directory whose
/// exact layout (group count, section names) is not the thing being tested.
fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn test_everything_a_written_database_leaves_on_disk_is_owner_only() {
    use crate::db::engine::Database;
    use crate::db::models::Record;
    use crate::test_support::isolated_config;

    let guard = TempDir::new("modes_storage");
    let base = guard.path();
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account("MODES", Some(base)).unwrap();
    db.logto("MODES").unwrap();
    db.create_table("SECRETS").unwrap();
    {
        let handle = db.get_table_mut("SECRETS").unwrap();
        let mut table = handle.write();
        table
            .records
            .insert("K1".to_string(), Record::from_bytes(b"correct horse battery staple"));
        table.touch_all();
    }
    db.save().unwrap();
    drop(db);

    let files = files_under(std::path::Path::new(base));
    assert!(!files.is_empty(), "the fixture must have written something");
    for path in files {
        assert_eq!(
            mode(&path),
            Some(0o600),
            "{} holds record data and must not be readable by anyone else",
            path.display()
        );
    }
}
