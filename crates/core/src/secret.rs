//! The one type any passphrase, key or token is held in.
//!
//! The rule this enforces is that leaking a secret has to be *deliberate*. A
//! `Secret` has no `Display`, no `Serialize` and no `Clone`; its `Debug` prints
//! `[redacted]`, so it is inert in a `{:?}` that reaches `$LOGS`, stderr or a
//! protocol error message. Getting the value out means calling [`expose`], which
//! is named so that the one place it is legitimate - handing a freshly issued
//! credential to the caller that asked for it - is visible in review as the
//! exception rather than lost among ordinary field accesses.
//!
//! It zeroizes on drop. That narrows a window rather than closing it: a value
//! copied out through `expose` is an ordinary `String` the allocator may reuse,
//! and anything with the process's memory has the secret regardless - which the
//! threat model in `docs/security.md` already puts out of scope.
//!
//! [`expose`]: Secret::expose

use std::io;
use zeroize::Zeroize;

/// A string nobody should be able to leak by accident.
pub struct Secret(String);

impl Secret {
    /// Takes ownership of a value that is already secret.
    pub fn new(value: String) -> Self {
        Secret(value)
    }

    /// `bytes` bytes from the system's entropy source, hex encoded.
    ///
    /// Falls back to `openssl rand` - already a hard dependency for certificate
    /// handling - rather than to anything time-derived, because a predictable
    /// secret is worse than an absent one: it looks like protection.
    pub fn random_hex(bytes: usize) -> io::Result<Self> {
        use std::io::Read;
        if let Ok(mut source) = std::fs::File::open("/dev/urandom") {
            let mut buffer = vec![0u8; bytes];
            if source.read_exact(&mut buffer).is_ok() {
                let hex = hex::encode(&buffer);
                buffer.zeroize();
                return Ok(Secret(hex));
            }
        }
        let output = std::process::Command::new("openssl")
            .args(["rand", "-hex", &bytes.to_string()])
            .output()?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || value.len() != bytes * 2 {
            return Err(io::Error::other("Could not read from the system entropy source"));
        }
        Ok(Secret(value))
    }

    /// The value itself. Every call site is a decision to let it out.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether `candidate` is this secret, in time that does not depend on how
    /// much of it is right - so a wrong guess cannot be narrowed down by how
    /// long the rejection took.
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let provided = candidate.as_bytes();
        if expected.len() != provided.len() {
            return false;
        }
        let mut difference = 0u8;
        for (a, b) in expected.iter().zip(provided.iter()) {
            difference |= a ^ b;
        }
        difference == 0
    }

    /// Whether there is anything here at all. Safe to ask: a length is not a
    /// value, and a caller has to be able to reject an empty configured secret.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `[redacted]`, so a `{:?}` on anything containing one is safe by construction.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
