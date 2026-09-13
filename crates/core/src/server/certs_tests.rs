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

#[test]
fn test_the_openssl_date_format_is_pinned() {
    // The exact bytes openssl prints, including the two-space padding on a
    // single-digit day. If a future openssl changed this, every expiry the
    // system reports would go quietly absent, so the format is asserted here
    // rather than trusted.
    assert_eq!(
        certs::parse_not_after_for_test("notAfter=Oct  6 20:16:55 2026 GMT"),
        Some("2026-10-06T20:16:55Z".to_string())
    );
    assert_eq!(
        certs::parse_not_after_for_test("notAfter=Sep 16 20:16:52 2026 GMT"),
        Some("2026-09-16T20:16:52Z".to_string())
    );
    // Anything else is "unknown", never a guess.
    assert_eq!(certs::parse_not_after_for_test("Oct  6 20:16:55 2026 GMT"), None);
    assert_eq!(certs::parse_not_after_for_test("notAfter=nonsense"), None);
    assert_eq!(certs::parse_not_after_for_test(""), None);
}

#[test]
fn test_an_issued_certificate_reports_when_it_expires() {
    if !openssl_present() {
        return;
    }
    let guard = TempDir::new("certs_expiry");
    let config = config_in(&guard);
    certs::ensure_certificates(&config).unwrap();

    let issued = certs::generate_client_cert(&config, "short-lived", 1, false).unwrap();
    let expires = issued.expires_at.expect("an issued certificate knows its expiry");

    // Read straight out of the certificate on disk, so this checks the value
    // reported to the caller against the one a client will be judged by.
    let from_disk = std::process::Command::new("openssl")
        .args(["x509", "-enddate", "-noout", "-in", &issued.cert_path])
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&from_disk.stdout).trim().to_string();
    assert_eq!(
        certs::parse_not_after_for_test(&printed),
        Some(expires.clone()),
        "the reported expiry must be the certificate's own"
    );

    // A one-day certificate expires within about a day, which is the property
    // the whole feature exists for.
    let long = certs::generate_client_cert(&config, "long-lived", 365, false).unwrap();
    assert!(
        long.expires_at.unwrap() > expires,
        "a longer lifetime must produce a later expiry"
    );
}

/// The CA rotation, end to end against real openssl (#60).
///
/// A rotation is only useful if it has an overlap: every client certificate is
/// signed by one CA, so replacing it invalidates all of them at once unless the
/// retiring CA stays trusted while clients are reissued one at a time.
#[test]
fn test_two_cas_can_be_trusted_at_once_while_clients_are_reissued() {
    if !openssl_present() {
        return;
    }
    let guard = TempDir::new("ca_rotation");
    let root = std::path::Path::new(guard.path());

    // The deployment as it stands: one CA, a server certificate, one client.
    let mut config = config_in(&guard);
    certs::ensure_certificates(&config).unwrap();
    let old_ca = config.ca_path.clone().unwrap();
    let old_client = certs::generate_client_cert(&config, "old-client", 30, false).unwrap();

    // Rotation step one: a second CA exists, and `ca_path` names it while the
    // outgoing one stays trusted.
    let new_ca = root.join("certs/ca-new.crt").to_string_lossy().into_owned();
    let new_ca_key = root.join("certs/ca-new.key").to_string_lossy().into_owned();
    private_files::reserve(&new_ca_key).unwrap();
    let made = std::process::Command::new("openssl")
        .args([
            "req",
            "-new",
            "-x509",
            "-days",
            "30",
            "-nodes",
            "-newkey",
            "rsa:2048",
            "-keyout",
            &new_ca_key,
            "-out",
            &new_ca,
            "-subj",
            "/CN=SmartRustyPick Root CA 2",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ])
        .status()
        .unwrap();
    assert!(made.success(), "the fixture needs a second CA");

    config.ca_path = Some(new_ca.clone());
    config.additional_ca_paths = Some(vec![old_ca.clone()]);

    // Both are trusted, and the bundle handed to a client carries both - which
    // is what lets it verify the server whichever CA has signed it.
    let trusted = certs::load_trusted_cas(&config).unwrap();
    assert_eq!(trusted.len(), 2, "both CAs must be in the trust set");
    let bundle = certs::trusted_ca_pem(&config).unwrap();
    assert_eq!(
        bundle.matches("BEGIN CERTIFICATE").count(),
        2,
        "the client bundle must carry both CAs"
    );

    // The server certificate is re-signed against the incoming CA, keeping its
    // key - otherwise the listener would present one no new client can verify.
    let key_before = std::fs::read(config.key_path.as_ref().unwrap()).unwrap();
    certs::ensure_certificates(&config).unwrap();
    assert_eq!(
        std::fs::read(config.key_path.as_ref().unwrap()).unwrap(),
        key_before,
        "a re-sign must keep the server's key"
    );
    assert!(
        verifies(config.cert_path.as_ref().unwrap(), &new_ca),
        "the server certificate must now chain to the incoming CA"
    );

    // The client issued before the rotation is still valid under the CA that
    // signed it, and a client issued now is signed by the incoming one. During
    // the overlap the listener trusts both, so neither is locked out.
    assert!(verifies(&old_client.cert_path, &old_ca));
    let new_client = certs::generate_client_cert(&config, "new-client", 30, false).unwrap();
    assert!(verifies(&new_client.cert_path, &new_ca));
    assert!(
        !verifies(&new_client.cert_path, &old_ca),
        "the fixture is only meaningful if the two CAs are actually different"
    );

    // Rotation step two: the outgoing CA is dropped once nothing is signed by
    // it. The old client is then no longer trusted, which is the point.
    config.additional_ca_paths = None;
    let trusted = certs::load_trusted_cas(&config).unwrap();
    assert_eq!(trusted.len(), 1);
    assert!(
        certs::trusted_ca_pem(&config)
            .unwrap()
            .matches("BEGIN CERTIFICATE")
            .count()
            == 1,
        "a retired CA must leave the client bundle too"
    );
}

/// Whether openssl can chain `cert` to `ca`.
fn verifies(cert: &str, ca: &str) -> bool {
    std::process::Command::new("openssl")
        .args(["verify", "-CAfile", ca, cert])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn test_a_ca_that_cannot_be_read_is_an_error_rather_than_a_silent_drop() {
    let guard = TempDir::new("ca_missing");
    let mut config = config_in(&guard);
    config.ca_path = Some(format!("{}/certs/absent.crt", guard.path()));
    // A CA that quietly failed to load is a set of clients that stop connecting
    // with nothing anywhere saying why, so it must fail loudly at startup.
    let refused = certs::load_trusted_cas(&config).unwrap_err();
    assert!(
        refused.to_string().contains("absent.crt"),
        "the error should name the file: {refused}"
    );

    let empty = format!("{}/certs/empty.crt", guard.path());
    private_files::dir(format!("{}/certs", guard.path())).unwrap();
    private_files::write(&empty, "").unwrap();
    config.additional_ca_paths = Some(vec![empty.clone()]);
    config.ca_path = None;
    let refused = certs::load_trusted_cas(&config).unwrap_err();
    assert!(refused.to_string().contains("no certificate"), "{refused}");
}
