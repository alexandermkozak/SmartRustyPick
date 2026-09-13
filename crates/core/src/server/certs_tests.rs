//! What `openssl` leaves behind, and who can read it.
//!
//! These tests shell out to `openssl` exactly as the code under test does, so
//! they are skipped rather than failed where it is absent - a machine without
//! it cannot generate certificates either, and a red test there would say
//! nothing about this code.

use crate::config::Config;
use crate::private_files;
use crate::server::certs;
use crate::test_support::TempDir;

fn openssl_present() -> bool {
    std::process::Command::new("openssl")
        .arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn config_in(dir: &TempDir) -> Config {
    let certs_dir = std::path::Path::new(dir.path()).join("certs");
    let at = |name: &str| certs_dir.join(name).to_string_lossy().into_owned();
    Config {
        web_enabled: Some(false),
        cert_path: Some(at("server.crt")),
        key_path: Some(at("server.key")),
        ca_path: Some(at("ca.crt")),
        ..Config::default()
    }
}

fn mode(path: &str) -> Option<u32> {
    private_files::mode_of(path).unwrap()
}

#[test]
fn test_generated_private_keys_are_owner_only() {
    if !openssl_present() {
        return;
    }
    let guard = TempDir::new("certs_modes");
    let config = config_in(&guard);
    certs::ensure_certificates(&config).unwrap();

    let certs_dir = std::path::Path::new(guard.path()).join("certs");
    // The directory listing is the set of clients this CA has issued for.
    assert_eq!(private_files::mode_of(&certs_dir).unwrap(), Some(0o700));

    let server_key = config.key_path.clone().unwrap();
    let ca_key = std::path::Path::new(guard.path())
        .join("certs/ca.key")
        .to_string_lossy()
        .into_owned();
    assert_eq!(mode(&server_key), Some(0o600), "server key");
    assert_eq!(mode(&ca_key), Some(0o600), "CA key");

    // A client certificate: the key and the PKCS#12 bundle both carry it.
    let issued = certs::generate_client_cert(&config, "modes-client", 1, true).unwrap();
    assert_eq!(mode(&issued.key_path), Some(0o600), "client key");
    let pfx = issued.pfx_path.expect("openssl is present, so the bundle is too");
    assert_eq!(mode(&pfx), Some(0o600), "PKCS#12 bundle");

    // The certificates themselves are public material, but they live in a
    // directory nobody else can open, so the whole tree is covered either way.
    assert!(std::path::Path::new(&issued.cert_path).exists());
}

/// Whether openssl can read the bundle with this `-passin` argument.
fn pkcs12_opens_with(pfx: &str, passin: &str) -> bool {
    std::process::Command::new("openssl")
        .args(["pkcs12", "-in", pfx, "-nokeys", "-passin", passin])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn test_the_pkcs12_bundle_needs_its_passphrase() {
    if !openssl_present() {
        return;
    }
    let guard = TempDir::new("certs_pfx");
    let config = config_in(&guard);
    certs::ensure_certificates(&config).unwrap();
    let issued = certs::generate_client_cert(&config, "bundle-client", 1, true).unwrap();

    let pfx = issued.pfx_path.clone().expect("a bundle");
    let passphrase = issued.pfx_passphrase.as_ref().expect("a passphrase with the bundle");

    assert!(
        pkcs12_opens_with(&pfx, &format!("pass:{}", passphrase.expose())),
        "the issued passphrase must open the bundle"
    );
    assert!(
        !pkcs12_opens_with(&pfx, "pass:"),
        "an empty password must not open the bundle - that was the bug"
    );
    assert!(!pkcs12_opens_with(&pfx, "pass:wrong"), "nor must a wrong one");
}

#[test]
fn test_the_passphrase_is_not_written_down_anywhere() {
    if !openssl_present() {
        return;
    }
    let guard = TempDir::new("certs_pfx_leak");
    let config = config_in(&guard);
    certs::ensure_certificates(&config).unwrap();
    let issued = certs::generate_client_cert(&config, "leak-client", 1, true).unwrap();
    let passphrase = issued
        .pfx_passphrase
        .as_ref()
        .expect("a passphrase")
        .expose()
        .to_string();

    // Every byte the issuance left on disk. The passphrase reached openssl
    // through the child's environment rather than its command line, and is
    // stored nowhere, so it must not appear in any of it.
    let mut stack = vec![std::path::PathBuf::from(guard.path())];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let haystack = String::from_utf8_lossy(&bytes);
            assert!(
                !haystack.contains(&passphrase),
                "{} contains the export passphrase",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "the fixture must have written something to check");

    // Nor through a Debug print of the struct that holds it.
    assert_eq!(format!("{:?}", issued.pfx_passphrase), "Some([redacted])");
}
