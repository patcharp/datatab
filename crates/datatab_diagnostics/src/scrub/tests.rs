use super::*;

fn clean(text: &str) -> String {
    match scrub(text) {
        ScrubOutcome::Clean(cleaned) => cleaned,
        ScrubOutcome::Dropped => panic!("expected a scrubbed value, got Dropped for {text:?}"),
    }
}

#[test]
fn scrubbing_clears_host_path_ip_email_and_a_password() {
    let message = "open /home/alice/reports/q3.csv failed for db.example.com at 10.1.2.3, user alice@example.com (postgres://alice:hunter2@db.example.com:5432/app)";

    let cleaned = clean(message);

    for secret in [
        "hunter2",
        "db.example.com",
        "/home/alice",
        "q3.csv",
        "10.1.2.3",
        "alice@example.com",
        "5432",
    ] {
        assert!(
            !cleaned.contains(secret),
            "{secret} survived scrubbing: {cleaned}"
        );
    }

    assert!(cleaned.contains("<path>"));
    assert!(cleaned.contains("<host>"));
    assert!(cleaned.contains("<ip>"));
    assert!(cleaned.contains("<email>"));
    assert!(cleaned.contains("<url>"));
}

#[test]
fn a_message_holding_a_query_is_dropped_whole() {
    for message in [
        "query failed: SELECT id, email FROM customers WHERE tenant = 'acme'",
        "insert into payments (card) values ('4111') failed",
        "update users set password = 'x' failed",
        "delete from audit_events failed",
    ] {
        assert_eq!(
            scrub(message),
            ScrubOutcome::Dropped,
            "{message} must not be sent"
        );
    }
}

#[test]
fn a_message_holding_a_host_port_is_cleared() {
    let cleaned = clean("dial failed to db.internal:5432 after 3 tries");

    assert!(!cleaned.contains("db.internal"));
    assert!(!cleaned.contains("5432"));
    assert!(cleaned.contains("<host>"));
}

#[test]
fn a_url_is_replaced_whole_including_its_credentials() {
    let cleaned = clean("GET https://user:hunter2@api.example.com/v1/reports?token=abc123 failed");

    assert!(!cleaned.contains("hunter2"));
    assert!(!cleaned.contains("api.example.com"));
    assert!(!cleaned.contains("token=abc123"));
    assert!(cleaned.contains("<url>"));
}

#[test]
fn ip_addresses_are_cleared_in_both_families() {
    let cleaned = clean("peer [::1] and fe80::1ff:fe23:4567:890a and 192.168.0.9 refused");

    assert!(!cleaned.contains("::1"));
    assert!(!cleaned.contains("fe80"));
    assert!(!cleaned.contains("192.168.0.9"));
    assert_eq!(cleaned.matches("<ip>").count(), 3);
}

#[test]
fn windows_paths_cross_spaces_and_lose_their_tails() {
    let cleaned = clean(r"cannot open C:\Program Files\DataTab\alice\q3.xlsx");

    assert!(!cleaned.contains("Program Files"));
    assert!(!cleaned.contains("alice"));
    assert!(!cleaned.contains("q3.xlsx"));
    assert_eq!(cleaned.matches("<path>").count(), 1);
}

#[test]
fn home_paths_are_cleared() {
    let cleaned = clean("cannot open ~/secret/notes.txt");

    assert!(!cleaned.contains("secret"));
    assert!(!cleaned.contains("notes.txt"));
    assert_eq!(cleaned.matches("<path>").count(), 1);
}

#[test]
fn paths_with_non_ascii_names_or_spaces_lose_their_tails() {
    let non_ascii = clean("cannot open /home/höz/x.csv");
    assert!(!non_ascii.contains("höz"), "{non_ascii}");
    assert!(!non_ascii.contains("x.csv"), "{non_ascii}");

    let spaced = clean("cannot open /var/lib/postgresql data/x");
    assert!(!spaced.contains("postgresql"), "{spaced}");
    assert!(!spaced.contains("data/x"), "{spaced}");
}

#[test]
fn a_relative_path_is_cleared() {
    let cleaned = clean("cannot read conf/local.toml");

    assert!(!cleaned.contains("conf"));
    assert!(!cleaned.contains("local.toml"));
    assert!(cleaned.contains("<path>"));
}

#[test]
fn a_single_label_host_with_a_port_is_cleared() {
    let cleaned = clean("dial postgres:5432 refused");

    assert!(!cleaned.contains("postgres"), "{cleaned}");
    assert!(!cleaned.contains("5432"), "{cleaned}");
    assert!(cleaned.contains("<host>"));
}

#[test]
fn a_dsn_pair_loses_its_values() {
    let cleaned = clean("connect failed: host=db.internal user=alice database=app port=5432");

    for value in ["db.internal", "alice", "app", "5432"] {
        assert!(!cleaned.contains(value), "{value} survived: {cleaned}");
    }
    assert!(cleaned.contains("host=<value>"));
    assert!(cleaned.contains("user=<value>"));
}

#[test]
fn quoted_dsn_values_lose_their_tails() {
    for message in [
        "host='dbhost' user='alice'",
        "host=\"dbhost\" user=\"alice\"",
    ] {
        let cleaned = clean(message);

        assert!(!cleaned.contains("dbhost"), "{message} leaked: {cleaned}");
        assert!(!cleaned.contains("alice"), "{message} leaked: {cleaned}");
        assert!(cleaned.contains("=<value>"), "{cleaned}");
    }
}

#[test]
fn a_driver_naming_a_user_is_cleared() {
    for message in [
        "pq: password authentication failed for user \"alice\"",
        "pq: authentication failed for user alice",
        "user \"alice\" failed",
    ] {
        let cleaned = clean(message);

        assert!(!cleaned.contains("alice"), "{message} leaked: {cleaned}");
        assert!(cleaned.contains("<value>"), "{cleaned}");
    }
}

#[test]
fn a_driver_naming_a_host_is_cleared() {
    for message in [
        "could not translate host name \"dbhost\" to address",
        "host \"dbhost\" refused",
    ] {
        let cleaned = clean(message);

        assert!(!cleaned.contains("dbhost"), "{message} leaked: {cleaned}");
        assert!(cleaned.contains("<value>"), "{cleaned}");
    }
}

#[test]
fn a_source_file_reference_reads_as_a_path() {
    let cleaned = clean("panic at main.rs:184");

    assert!(!cleaned.contains("main.rs"), "{cleaned}");
    assert!(!cleaned.contains("184"), "{cleaned}");
    assert!(!cleaned.contains("<host>"), "{cleaned}");
    assert_eq!(cleaned, "panic at <path>");

    let cleaned = clean("cannot read Cargo.toml");
    assert_eq!(cleaned, "cannot read <path>");
}

#[test]
fn an_underscore_prefixed_dsn_key_loses_its_value() {
    let cleaned = clean("DB_HOST=db.internal PGHOST=db2.internal refused");

    assert!(!cleaned.contains("db.internal"), "{cleaned}");
    assert!(!cleaned.contains("db2.internal"), "{cleaned}");
}

#[test]
fn prefixed_credentials_are_dropped() {
    for message in [
        "PGPASSWORD=hunter2",
        "DB_PASSWORD=hunter2",
        "app_pwd: hunter2",
    ] {
        assert_eq!(
            scrub(message),
            ScrubOutcome::Dropped,
            "{message} must not be sent"
        );
    }
}

#[test]
fn json_credentials_are_dropped() {
    for message in [r#"{"password":"hunter2"}"#, r#"{"token":"abc123"}"#] {
        assert_eq!(
            scrub(message),
            ScrubOutcome::Dropped,
            "{message} must not be sent"
        );
    }
}

#[test]
fn a_bearer_token_is_cleared_even_when_short() {
    let cleaned = clean("request failed with Authorization: Bearer abc");

    assert!(!cleaned.contains("abc"), "{cleaned}");
    assert!(cleaned.contains("<redacted>"));
}

#[test]
fn a_credential_the_redactor_cannot_clear_is_dropped() {
    assert_eq!(
        scrub("auth failed: password=hunter2"),
        ScrubOutcome::Dropped,
        "a plain credential assignment must not be sent"
    );
}

#[test]
fn plain_text_passes_through_unchanged() {
    assert_eq!(
        clean("assertion failed: left == right"),
        "assertion failed: left == right"
    );
}

#[test]
fn long_text_is_cut_at_a_character_boundary() {
    let text = "é".repeat(MAX_FREE_TEXT_BYTES);

    let cleaned = clean(&text);

    assert!(cleaned.ends_with("<truncated>"));
    assert!(cleaned.len() <= MAX_FREE_TEXT_BYTES + "<truncated>".len());
    assert!(cleaned.starts_with('é'));
}

#[test]
fn empty_text_stays_empty() {
    assert_eq!(scrub("   "), ScrubOutcome::Clean(String::new()));
}
