//! Tests for the dashboard's own decisions: where it looks for the database,
//! what it accepts as a token, and what it hands to a caller without one.

use super::*;
use std::collections::HashMap;

fn request(path: &str, headers: &[(&str, &str)], query: &[(&str, &str)]) -> http::Request {
    http::Request {
        method: "GET".to_string(),
        path: path.to_string(),
        query: query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        headers: headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())).collect::<HashMap<_, _>>(),
        body: Vec::new(),
        keep_alive: true,
    }
}

#[test]
fn a_wildcard_bind_is_reached_over_the_loopback_interface() {
    // The server certificate names localhost, and a dashboard on the same host
    // has no reason to go out and back.
    assert_eq!(loopback_target("0.0.0.0:8443"), "127.0.0.1:8443");
    assert_eq!(loopback_target("[::]:8443"), "127.0.0.1:8443");
    assert_eq!(loopback_target("127.0.0.1:9999"), "127.0.0.1:9999");
    assert_eq!(loopback_target("192.168.1.10:8443"), "192.168.1.10:8443");
}

#[test]
fn a_bind_address_is_classified_by_who_can_reach_it() {
    // Which of these two an address is decides what the startup line says, and
    // the mismatch - database on every interface, dashboard on loopback - is
    // exactly what makes a containerised dashboard look broken.
    assert!(is_wildcard("0.0.0.0:8443"));
    assert!(is_wildcard("[::]:8443"));
    assert!(!is_wildcard("127.0.0.1:8443"));
    assert!(!is_wildcard("192.168.1.10:8443"));

    assert!(is_loopback("127.0.0.1:8080"));
    assert!(is_loopback("127.0.0.53:8080"));
    assert!(is_loopback("localhost:8080"));
    assert!(is_loopback("[::1]:8080"));
    assert!(!is_loopback("0.0.0.0:8080"));
    assert!(!is_loopback("192.168.1.10:8080"));
}

#[test]
fn tokens_have_to_match_exactly() {
    assert!(tokens_match("abc123", "abc123"));
    assert!(!tokens_match("abc123", "abc124"));
    assert!(!tokens_match("abc123", "abc12"));
    assert!(!tokens_match("abc123", ""));
}

#[test]
fn generated_tokens_are_long_and_unique() {
    let first = random_token().expect("a token can be generated");
    let second = random_token().expect("a token can be generated");
    assert!(first.len() >= 32, "token is too short to be unguessable: {}", first.len());
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(first, second);
}

#[test]
fn the_token_is_accepted_from_a_cookie_a_bearer_header_or_the_url() {
    let token = "s3cret";
    assert!(authenticated(&request("/", &[("Cookie", "srp_token=s3cret")], &[]), token));
    assert!(authenticated(&request("/", &[("Authorization", "Bearer s3cret")], &[]), token));
    assert!(authenticated(&request("/", &[], &[("token", "s3cret")]), token));

    assert!(!authenticated(&request("/", &[], &[]), token));
    assert!(!authenticated(&request("/", &[("Cookie", "srp_token=wrong")], &[]), token));
    assert!(!authenticated(&request("/", &[("Authorization", "Basic s3cret")], &[]), token));
    assert!(!authenticated(&request("/", &[], &[("token", "s3cre")]), token));
}

#[test]
fn an_unauthenticated_api_call_is_json_and_a_page_request_is_readable() {
    let api = unauthorized(&request("/api/stats", &[], &[]));
    assert_eq!(api.status, 401);
    assert!(api.content_type.starts_with("application/json"));

    let page = unauthorized(&request("/", &[], &[]));
    assert_eq!(page.status, 401);
    assert!(page.content_type.starts_with("text/html"));
    assert!(String::from_utf8_lossy(&page.body).contains("token"));
}

#[test]
fn the_page_only_loads_assets_this_server_serves() {
    // The strict Content-Security-Policy in `http::write_response` blocks
    // anything remote, so a reference to one would be a blank page in the
    // browser and nothing at all in the tests.
    assert!(!INDEX_HTML.contains("http://"), "the page must not load anything over plain HTTP");
    assert!(!INDEX_HTML.contains("https://"), "the page must not load remote assets");
    assert!(INDEX_HTML.contains("/app.css") && INDEX_HTML.contains("/app.js"));
    assert!(!INDEX_HTML.contains("<script>"), "inline scripts are refused by the policy");
    assert!(!APP_JS.is_empty() && !APP_CSS.is_empty());
}

#[test]
fn every_view_the_page_offers_has_a_section_to_show() {
    for view in ["overview", "clients", "certificates", "accounts"] {
        assert!(
            INDEX_HTML.contains(&format!("data-view=\"{}\"", view)),
            "no tab for the {} view",
            view
        );
        assert!(
            INDEX_HTML.contains(&format!("id=\"view-{}\"", view)),
            "no section for the {} view",
            view
        );
    }
}
