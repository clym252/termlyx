//! `known_hosts` parsing and host matching. The hashed lines below were
//! produced by OpenSSH's own `ssh-keygen -H` from
//! `hashed.example.com <key>` and `[hashed.example.com]:2222 <key>`.

use core_ssh::{KnownHostEntry, host_matches, parse_known_hosts};

const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIGg7FLBVOjX+CUiviEq6ggGvY1501+RdoGpVxtm3Cdu2";
const ECDSA: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBG6ZXrxPG8zhNFAioKaDbiNSTNxbHvSskPP3H4usxggTlTdbiYXka0da3yFmtNDAPcJKDraAMSRvizUIltGBXBA=";
const HASHED_22: &str = "|1|HaW8/opFzilgDxjQXnStU7xLnlk=|AAFGL7JKn8j+Jgphlt8Nu54tiFk=";
const HASHED_2222: &str = "|1|7wmNmyEG5VYJiCsJAU3d+Fl/AJ4=|WbqvqfIzLjoRnqeDprqPRum7k14=";

fn sample() -> String {
    format!(
        "# a comment line\n\
         \n\
         \x20  # indented comment\n\
         plain.example.com ssh-ed25519 {ED25519} a trailing comment\n\
         alias-one,Alias-Two,10.0.0.7 ecdsa-sha2-nistp256 {ECDSA}\n\
         [bracket.example.com]:2222 ssh-ed25519 {ED25519}\n\
         {HASHED_22} ssh-ed25519 {ED25519}\n\
         {HASHED_2222} ssh-ed25519 {ED25519}\n\
         @cert-authority *.example.com ssh-ed25519 {ED25519}\n\
         @revoked revoked.example.com ssh-ed25519 {ED25519}\n\
         *.corp,!bad.corp ssh-ed25519 {ED25519}\n\
         broken.example.com ssh-ed25519\n\
         garbage.example.com ssh-ed25519 not-base64!!\n\
         mismatch.example.com ssh-rsa {ED25519}\n\
         [noport.example.com] ssh-ed25519 {ED25519}\n"
    )
}

fn entry(pattern: &str, port: u16, algorithm: &str, key: &str) -> KnownHostEntry {
    KnownHostEntry {
        host_pattern: pattern.into(),
        port,
        algorithm: algorithm.into(),
        public_key: key.into(),
    }
}

#[test]
fn parses_hashed_and_bracketed_known_hosts() {
    let entries = parse_known_hosts(&sample());
    assert_eq!(
        entries,
        vec![
            entry("plain.example.com", 22, "ssh-ed25519", ED25519),
            entry("alias-one", 22, "ecdsa-sha2-nistp256", ECDSA),
            entry("alias-two", 22, "ecdsa-sha2-nistp256", ECDSA),
            entry("10.0.0.7", 22, "ecdsa-sha2-nistp256", ECDSA),
            entry("bracket.example.com", 2222, "ssh-ed25519", ED25519),
            entry(HASHED_22, 0, "ssh-ed25519", ED25519),
            entry(HASHED_2222, 0, "ssh-ed25519", ED25519),
        ]
    );

    // Hashed names match the way OpenSSH hashes them: `host` for port 22,
    // `[host]:port` otherwise.
    assert!(host_matches(HASHED_22, 0, "hashed.example.com", 22));
    assert!(host_matches(HASHED_22, 0, "HASHED.example.com", 22));
    assert!(!host_matches(HASHED_22, 0, "hashed.example.com", 2222));
    assert!(!host_matches(HASHED_22, 0, "other.example.com", 22));
    assert!(host_matches(HASHED_2222, 0, "hashed.example.com", 2222));
    assert!(!host_matches(HASHED_2222, 0, "hashed.example.com", 22));
}

#[test]
fn known_host_patterns_match_host_and_port() {
    // Plain names compare case-insensitively and need the stored port.
    assert!(host_matches(
        "plain.example.com",
        22,
        "Plain.Example.com",
        22
    ));
    assert!(!host_matches(
        "plain.example.com",
        22,
        "plain.example.com",
        2222
    ));
    assert!(host_matches("127.0.0.1", 2222, "127.0.0.1", 2222));
    // `[host]:port` carries its own port.
    assert!(host_matches(
        "[bracket.example.com]:2222",
        0,
        "bracket.example.com",
        2222
    ));
    assert!(!host_matches(
        "[bracket.example.com]:2222",
        0,
        "bracket.example.com",
        22
    ));
    assert!(host_matches("[::1]:2200", 0, "::1", 2200));
    // Globs.
    assert!(host_matches("*.corp", 22, "db.corp", 22));
    assert!(host_matches("web-?.corp", 22, "web-1.corp", 22));
    assert!(!host_matches("web-?.corp", 22, "web-10.corp", 22));
    assert!(!host_matches("*.corp", 22, "corp.evil", 22));
    // Malformed hashes never match.
    assert!(!host_matches("|1|bad|bad", 0, "hashed.example.com", 22));
    assert!(!host_matches("|1|", 0, "", 22));
}
