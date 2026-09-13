//! The properties that make `Secret` worth having: it cannot be printed, it
//! cannot be compared in variable time, and two of them are never the same.

use crate::secret::Secret;

#[test]
fn test_a_secret_does_not_print_itself() {
    let secret = Secret::new("correct horse battery staple".to_string());
    assert_eq!(format!("{:?}", secret), "[redacted]");
    // The same holds one level up, which is the case that actually bites: a
    // struct deriving Debug around a Secret is the shape that reaches a log.
    #[derive(Debug)]
    struct Carrier {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        passphrase: Secret,
    }
    let carrier = Carrier {
        name: "client".to_string(),
        passphrase: Secret::new("hunter2".to_string()),
    };
    let printed = format!("{:?}", carrier);
    assert!(printed.contains("[redacted]"), "{printed}");
    assert!(!printed.contains("hunter2"), "{printed}");
}

#[test]
fn test_random_secrets_are_the_requested_size_and_never_repeat() {
    let first = Secret::random_hex(18).unwrap();
    let second = Secret::random_hex(18).unwrap();
    assert_eq!(first.expose().len(), 36, "18 bytes is 36 hex characters");
    assert!(first.expose().chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(
        first.expose(),
        second.expose(),
        "two issuances must not share a passphrase"
    );
}

#[test]
fn test_matching_is_exact() {
    let secret = Secret::new("abc123".to_string());
    assert!(secret.matches("abc123"));
    assert!(!secret.matches("abc124"));
    assert!(!secret.matches("abc12"), "a prefix is not a match");
    assert!(!secret.matches("abc1234"), "nor is an extension");
    assert!(!secret.matches(""));
}

#[test]
fn test_an_empty_secret_is_recognisable_without_being_read() {
    assert!(Secret::new(String::new()).is_empty());
    assert!(!Secret::new("x".to_string()).is_empty());
}
