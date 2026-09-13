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
