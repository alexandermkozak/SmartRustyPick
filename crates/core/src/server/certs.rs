use crate::config::Config;
use crate::private_files;
use crate::secret::Secret;
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::fs::File;
use std::io::{self, BufReader as SyncBufReader};
use std::path::{Path, PathBuf};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A file next to `path`, with `path`'s file stem and the given extension.
///
/// Every generated artefact stays beside the certificate it belongs to, so pointing
/// the config at a directory (`.local/certs/server.crt`) keeps the CA key, the CSR and
/// the extension file out of the working directory as well.
fn sibling(path: &str, extension: &str) -> String {
    let mut sibling = PathBuf::from(path);
    sibling.set_extension(extension);
    sibling.to_string_lossy().into_owned()
}

/// Creates the directory a generated artefact lives in, owner-only.
///
/// `0700` rather than `0600`-files-in-a-public-directory because the entries are
/// named after their common names: the listing alone is the set of clients this
/// CA has ever issued for.
fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => private_files::dir(parent),
        _ => Ok(()),
    }
}

pub fn ensure_certificates(config: &Config) -> std::io::Result<()> {
    let cert_path = config.cert_path.as_ref().expect("cert_path missing");
    let key_path = config.key_path.as_ref().expect("key_path missing");
    let ca_path = config.ca_path.as_ref().expect("ca_path missing");
    let ca_key_path = &sibling(ca_path, "key"); // Private key for CA

    let cert_exists = Path::new(cert_path).exists();
    let key_exists = Path::new(key_path).exists();
    let ca_exists = Path::new(ca_path).exists();

    if cert_exists && key_exists && ca_exists {
        return Ok(());
    }

    println!("Generating certificates for first-time startup...");

    for path in [cert_path, key_path, ca_path, ca_key_path] {
        ensure_parent_dir(path)?;
    }

    // 1. Generate CA key and certificate if needed
    if !Path::new(ca_key_path).exists() || !ca_exists {
        println!("Generating CA certificate...");
        // openssl truncates an existing `-keyout` rather than replacing it, so the
        // mode is decided here and there is no instant at which the CA key is
        // readable by anyone else.
        private_files::reserve(ca_key_path.as_str())?;
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-new",
                "-x509",
                "-days",
                "3650",
                "-nodes",
                "-newkey",
                "rsa:2048",
                "-keyout",
                ca_key_path.as_str(),
                "-out",
                ca_path,
                "-subj",
                "/CN=SmartRustyPick Root CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ])
            .status()?;
        if !status.success() {
            let _ = std::fs::remove_file(ca_key_path.as_str());
            return Err(std::io::Error::other("Failed to generate CA certificate"));
        }
    }

    // 2. Generate server key and CSR
    if !key_exists {
        println!("Generating server certificate...");
        let csr_path = &sibling(cert_path, "csr");
        private_files::reserve(key_path.as_str())?;
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-new",
                "-nodes",
                "-newkey",
                "rsa:2048",
                "-keyout",
                key_path,
                "-out",
                csr_path.as_str(),
                "-subj",
                "/CN=localhost",
            ])
            .status()?;
        if !status.success() {
            let _ = std::fs::remove_file(key_path.as_str());
            return Err(std::io::Error::other("Failed to generate server CSR"));
        }

        // 3. Sign server certificate with CA
        let ext_path = &sibling(cert_path, "ext");
        std::fs::write(
            ext_path,
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nsubjectAltName = DNS:localhost, IP:127.0.0.1",
        )?;

        let status = std::process::Command::new("openssl")
            .args([
                "x509",
                "-req",
                "-in",
                csr_path.as_str(),
                "-CA",
                ca_path,
                "-CAkey",
                ca_key_path.as_str(),
                "-CAcreateserial",
                "-out",
                cert_path,
                "-days",
                "365",
                "-sha256",
                "-extfile",
                ext_path.as_str(),
            ])
            .status()?;
        // The CSR and the extension file are inputs to the signing step only; keeping
        // them around just leaves scratch behind next to the certificate.
        let _ = std::fs::remove_file(csr_path);
        let _ = std::fs::remove_file(ext_path);
        if !status.success() {
            return Err(std::io::Error::other("Failed to sign server certificate"));
        }
    }

    Ok(())
}

/// A freshly signed client certificate and everything needed to use it.
///
/// The PEM bodies are carried alongside the paths so a caller that never
/// touches the server's filesystem - the dashboard handing a certificate to a
/// browser - can still deliver the material.
///
/// **Deliberately not `Serialize`, and deliberately not `Debug`.** This is the
/// one struct in the project that holds a private key and a passphrase at the
/// same time; a derive would put both into any response or log line that ever
/// touched it. [`record`](GeneratedCert::record) is the single way out, and it
/// names every field it emits, so the protocol shape is a written decision
/// rather than a consequence of the field order here.
pub struct GeneratedCert {
    pub common_name: String,
    pub thumbprint: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub ca_pem: String,
    pub cert_path: String,
    pub key_path: String,
    /// Present only when the PKCS#12 bundle could be produced.
    pub pfx_path: Option<String>,
    /// When the certificate stops being valid, as an RFC 3339 UTC timestamp,
    /// read from the certificate itself. `None` only if it could not be read;
    /// an absent expiry says "unknown" rather than "none".
    pub expires_at: Option<String>,
    /// The bundle's import passphrase, present exactly when `pfx_path` is.
    ///
    /// Generated per issuance, never written down, and delivered only through
    /// [`record`](GeneratedCert::record) to the caller that asked for the
    /// certificate. A bundle whose passphrase is stored beside it is decorative,
    /// so the only copy is the one the caller is handed.
    pub pfx_passphrase: Option<Secret>,
}

impl GeneratedCert {
    /// The issuance response, field by field.
    ///
    /// The private key and the passphrase are in here on purpose: this is the
    /// certificate-issuance path, which exists to deliver exactly that material
    /// and is the one exception `docs/security.md` carves out of "no secret
    /// reaches a response body".
    pub fn record(&self) -> serde_json::Value {
        serde_json::json!({
            "common_name": self.common_name,
            "thumbprint": self.thumbprint,
            "certificate_pem": self.certificate_pem,
            "private_key_pem": self.private_key_pem,
            "ca_pem": self.ca_pem,
            "cert_path": self.cert_path,
            "key_path": self.key_path,
            "pfx_path": self.pfx_path,
            "expires_at": self.expires_at,
            "pfx_passphrase": self.pfx_passphrase.as_ref().map(Secret::expose),
        })
    }
}

/// Names the environment variable openssl reads the export passphrase from.
///
/// `-passout pass:<value>` would put it on the command line, where `ps` shows it
/// to every user on the host. A child's environment is readable through
/// `/proc/<pid>/environ` by its owner alone, which is the same boundary the key
/// file itself sits behind.
const PFX_PASSPHRASE_VAR: &str = "SRP_PFX_PASSPHRASE";

/// 18 bytes - 144 bits as 36 hex characters. Hex rather than anything prettier
/// because the passphrase is read off a screen and typed into an import dialog,
/// and a character set with no ambiguity is worth more there than brevity.
const PFX_PASSPHRASE_BYTES: usize = 18;

/// Rejects names that would turn into an `openssl` option or escape the
/// certificate directory. The name reaches a command line and a file path, so
/// it is checked once, here, rather than at each call site.
pub fn validate_common_name(common_name: &str) -> Result<(), String> {
    if common_name.is_empty() {
        return Err("Common name not specified".to_string());
    }
    if common_name.len() > 64 {
        return Err("Common name must be 64 characters or fewer".to_string());
    }
    if common_name.starts_with('-')
        || common_name.contains('/')
        || common_name.contains('\\')
        || common_name.contains("..")
    {
        return Err("Invalid common name: must not start with '-' or contain path separators".to_string());
    }
    if !common_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err("Invalid common name: only letters, digits, '.', '-' and '_' are allowed".to_string());
    }
    Ok(())
}

/// SHA-256 thumbprint (lowercase hex) of the first certificate in a PEM body.
///
/// The same hash the TLS server computes over the peer's DER certificate, so a
/// certificate can be authorized before it has ever been presented.
pub fn thumbprint_of_pem(pem: &str) -> io::Result<String> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let der = certs(&mut reader)
        .next()
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "No certificate found in PEM"))?;
    Ok(thumbprint_of_der(&der))
}

/// SHA-256 thumbprint (lowercase hex) of a DER-encoded certificate.
pub fn thumbprint_of_der(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(der);
    hex::encode(hasher.finalize())
}

/// Where generated client certificates live: next to the CA that signs them.
fn cert_output_dir(ca_path: &str) -> PathBuf {
    Path::new(ca_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads the `notAfter` date out of a signed certificate, as an RFC 3339 UTC
/// timestamp.
///
/// Read from the certificate rather than computed as `now + days`, because the
/// certificate is what a client will actually be judged against. The two agree
/// today; if `openssl` ever interpreted `-days` differently, a computed date
/// would report an expiry the deployment does not have - and a wrong expiry is
/// precisely the outage that reporting it at all is meant to prevent.
///
/// `None` when the date cannot be read. An absent expiry says "unknown", which
/// is honest; a guessed one is not.
fn not_after(cert_path: &str) -> Option<String> {
    let output = std::process::Command::new("openssl")
        .args(["x509", "-enddate", "-noout", "-in", cert_path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse_not_after(line.trim())
}

/// The parser, reachable from the tests that pin openssl's output format.
#[cfg(test)]
pub fn parse_not_after_for_test(line: &str) -> Option<String> {
    parse_not_after(line)
}

/// `notAfter=Oct  6 20:16:55 2026 GMT` to `2026-10-06T20:16:55Z`.
///
/// Split out from [`not_after`] so the format can be pinned by a test without
/// generating a certificate. Note the day is **space padded** - openssl prints
/// `Oct  6`, with two spaces - which is why this is a real format description
/// and not a `split_whitespace`.
fn parse_not_after(line: &str) -> Option<String> {
    const OPENSSL_DATE: &[time::format_description::BorrowedFormatItem<'_>] = time::macros::format_description!(
        "[month repr:short case_sensitive:false] [day padding:space] [hour]:[minute]:[second] [year] GMT"
    );
    let value = line.strip_prefix("notAfter=")?;
    let parsed = time::PrimitiveDateTime::parse(value, OPENSSL_DATE).ok()?;
    parsed
        .assume_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// Issues a client certificate signed by the configured CA.
///
/// `days` bounds the certificate's life, which is what lets a caller mint a
/// deliberately short-lived one. `write_pfx` adds the PKCS#12 bundle the CLI
/// hands to GUI clients; its failure is not fatal, since the PEM pair is what
/// the protocol actually needs.
///
/// The bundle carries the CA as well as the leaf and the key. A leaf-only PKCS#12
/// still parses, but a client that picks its certificate by building a chain -
/// Windows' Schannel does - will not offer one it cannot chain to the CA the
/// server asked for, and the connection is then dropped as unauthenticated.
pub fn generate_client_cert(
    config: &Config,
    common_name: &str,
    days: u32,
    write_pfx: bool,
) -> io::Result<GeneratedCert> {
    if let Err(message) = validate_common_name(common_name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
    }

    let ca_file = config.ca_path.clone().unwrap_or_else(|| "ca.crt".to_string());
    let ca_key_file = sibling(&ca_file, "key");
    if !Path::new(&ca_file).exists() || !Path::new(&ca_key_file).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("CA material missing: expected {} and {}", ca_file, ca_key_file),
        ));
    }

    let out_dir = cert_output_dir(&ca_file);
    private_files::dir(&out_dir)?;
    let out = |extension: &str| {
        out_dir
            .join(format!("{}.{}", common_name, extension))
            .to_string_lossy()
            .into_owned()
    };
    let key_file = out("key");
    let csr_file = out("csr");
    let crt_file = out("crt");
    let pfx_file = out("pfx");
    let ext_file = out("ext");

    let failed = |step: &str| io::Error::other(format!("{} failed", step));
    let ran = |result: io::Result<std::process::ExitStatus>| matches!(result, Ok(status) if status.success());

    // The private key never leaves this directory except through the caller, and
    // it is owner-only before openssl has written a byte into it - `genrsa -out`
    // truncates the file this reserves instead of creating one at the umask.
    private_files::reserve(&key_file)?;
    if !ran(std::process::Command::new("openssl")
        .args(["genrsa", "-out", &key_file, "2048"])
        .status())
    {
        let _ = std::fs::remove_file(&key_file);
        return Err(failed("Generating the RSA key"));
    }

    let subject = format!("/CN={}", common_name);
    if !ran(std::process::Command::new("openssl")
        .args(["req", "-new", "-key", &key_file, "-out", &csr_file, "-subj", &subject])
        .status())
    {
        let _ = std::fs::remove_file(&csr_file);
        return Err(failed("Generating the certificate request"));
    }

    let mut extensions = String::from(
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\nsubjectAltName = DNS:",
    );
    extensions.push_str(common_name);
    if common_name == "localhost" {
        extensions.push_str(", IP:127.0.0.1");
    }
    std::fs::write(&ext_file, extensions)?;

    let days = days.max(1).to_string();
    let signed = ran(std::process::Command::new("openssl")
        .args([
            "x509",
            "-req",
            "-in",
            &csr_file,
            "-CA",
            &ca_file,
            "-CAkey",
            &ca_key_file,
            "-CAcreateserial",
            "-out",
            &crt_file,
            "-days",
            &days,
            "-sha256",
            "-extfile",
            &ext_file,
        ])
        .status());
    // Inputs to the signing step only; leaving them behind is just litter.
    let _ = std::fs::remove_file(&csr_file);
    let _ = std::fs::remove_file(&ext_file);
    if !signed {
        return Err(failed("Signing the certificate"));
    }

    // The bundle and its passphrase are produced together or not at all: a
    // `.pfx` whose passphrase nobody holds is not a credential, and a passphrase
    // for a bundle that was never written is a puzzle for whoever reads the
    // response.
    let bundle = if write_pfx {
        let passphrase = Secret::random_hex(PFX_PASSPHRASE_BYTES)?;
        let exported = private_files::reserve(&pfx_file).is_ok()
            && ran(std::process::Command::new("openssl")
                .args([
                    "pkcs12",
                    "-export",
                    "-out",
                    &pfx_file,
                    "-inkey",
                    &key_file,
                    "-in",
                    &crt_file,
                    "-certfile",
                    &ca_file,
                    "-passout",
                    &format!("env:{}", PFX_PASSPHRASE_VAR),
                ])
                .env(PFX_PASSPHRASE_VAR, passphrase.expose())
                .status());
        if exported {
            Some((pfx_file, passphrase))
        } else {
            // A reserved-but-unwritten bundle is an empty file claiming to be a
            // credential; the caller is told there is none, so there must be none.
            let _ = std::fs::remove_file(&pfx_file);
            None
        }
    } else {
        None
    };
    let (pfx_path, pfx_passphrase) = match bundle {
        Some((path, passphrase)) => (Some(path), Some(passphrase)),
        None => (None, None),
    };

    let certificate_pem = std::fs::read_to_string(&crt_file)?;
    let private_key_pem = std::fs::read_to_string(&key_file)?;
    let ca_pem = std::fs::read_to_string(&ca_file)?;
    let thumbprint = thumbprint_of_pem(&certificate_pem)?;

    Ok(GeneratedCert {
        common_name: common_name.to_string(),
        thumbprint,
        certificate_pem,
        private_key_pem,
        ca_pem,
        expires_at: not_after(&crt_file),
        cert_path: crt_file,
        key_path: key_file,
        pfx_path,
        pfx_passphrase,
    })
}

pub fn load_certs(path: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    certs(&mut reader).collect()
}

pub fn load_key(path: &str) -> io::Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = SyncBufReader::new(file);
    let keys = pkcs8_private_keys(&mut reader).collect::<io::Result<Vec<_>>>()?;
    if keys.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "No private key found"));
    }
    Ok(PrivateKeyDer::Pkcs8(keys[0].clone_key()))
}
