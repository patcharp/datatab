use super::super::*;

/// Every secret value in these tests contains this text.
const SECRET: &str = "hunter";

/// A real wire error for the branch that echoes the whole connection string.
fn wire_parse_error() -> postgres::Error {
    "unknown_option=1"
        .parse::<postgres::Config>()
        .expect_err("the wire parser knows no such option")
}

fn message_of(error: DbError) -> String {
    match error {
        DbError::ConnectionFailed(formatted) => formatted.message,
        other => panic!("unexpected error: {other}"),
    }
}

/// The message is also the text of the `log::error!` line.
fn uri_error_message(input: &str) -> String {
    message_of(format_redshift_uri_error(&wire_parse_error(), input))
}

fn connect_error_message(input: &str) -> String {
    match RedshiftDriver::new().connect_with_uri(input, None, &RedshiftTlsCerts::default()) {
        Err(error) => message_of(error),
        Ok(_) => panic!("the connection must fail"),
    }
}

#[test]
fn keyword_connect_error_hides_every_secret_value() {
    for input in [
        "host=h password=hunter2",
        "host=h password='hunter 2'",
        r"host=h password = 'hunter\' 2' dbname=app",
        r"host=h password=hunter\ 2 dbname=app",
        "host=h sslpassword=hunter2",
        "host=h PASSWORD=hunter2",
        "host=h password=hunter2 password=hunter3",
        "host=h oauth_client_secret=hunter2",
        "host=h scram_client_key=hunter2 scram_server_key=hunter3",
    ] {
        let message = uri_error_message(input);
        assert!(!message.contains(SECRET), "{message}");
        assert!(message.contains("host=h"), "{message}");
        assert!(message.contains("=***"), "{message}");
    }
}

#[test]
fn keyword_connect_error_keeps_other_keys_readable() {
    let message = uri_error_message(
        "host=db.example.com port=5433 user = app dbname='my db' password=hunter2 \
         sslkey=client-key.pem passfile=pgpass",
    );
    assert_eq!(
        message,
        "Invalid connection URI format: host=db.example.com port=5433 user = app \
         dbname='my db' password=*** sslkey=client-key.pem passfile=pgpass"
    );
}

#[test]
fn unreadable_keyword_string_shows_no_secret() {
    // The pass masks an open quote to the end of the text and keeps the quote.
    assert_eq!(
        uri_error_message("host=h password='hunter 2"),
        "Invalid connection URI format: [connection string hidden]"
    );
    // The leftover `2` has no key, so no part of the string is safe to show.
    assert_eq!(
        uri_error_message("host=h password=hunter 2"),
        "Invalid connection URI format: [connection string hidden]"
    );
}

#[test]
fn url_connect_error_still_redacts_credentials() {
    for (input, expected) in [
        (
            "postgresql://localhost:5432/app?password=hunter2&sslmode=require",
            "postgresql://localhost:5432/app?password=***",
        ),
        (
            "redshift://localhost:5439/dev?password=hunter2",
            "redshift://localhost:5439/dev?password=***",
        ),
        // Another scheme reads as one keyword parameter: its URL redaction stays.
        (
            "mysql://localhost/app?password=hunter2",
            "mysql://localhost/app?password=***",
        ),
    ] {
        assert_eq!(
            uri_error_message(input),
            format!("Invalid connection URI format: {expected}")
        );
    }
}

#[test]
fn build_uri_config_hides_a_secret_the_wire_parser_rejects() {
    // The wire parser refuses `sslpassword` before any network access.
    let message = match build_uri_config("host=h sslpassword=hunter2", None) {
        Err(error) => message_of(error),
        Ok(_) => panic!("the wire parser must refuse the string"),
    };
    assert_eq!(
        message,
        "Invalid connection URI format: host=h sslpassword=***"
    );
}

#[test]
fn refused_keyword_connection_hides_the_password() {
    // The listener stays bound and closes the one connection it accepts, so
    // the client always meets a live socket that hangs up: no port is freed
    // for another process to take.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("local address").port();
    let hang_up = std::thread::spawn(move || drop(listener.accept()));

    let message = connect_error_message(&format!(
        "host=127.0.0.1 port={port} user=app password=hunter2 sslmode=disable connect_timeout=5"
    ));
    hang_up.join().expect("the listener thread ends");
    assert!(!message.contains(SECRET), "{message}");
    let shown = format!(
        "Connection error with URI host=127.0.0.1 port={port} user=app password=*** \
         sslmode=disable connect_timeout=5: "
    );
    assert!(message.starts_with(&shown), "{message}");
}

#[test]
fn unplaceable_secret_shapes_are_hidden_in_the_error() {
    for (input, secrets) in [
        ("host=h 'password=hunter2'", &["hunter2"][..]),
        ("host=h;password=hunter2", &["hunter2"]),
        (
            "postgresql://db.example.com/db password=hunter2",
            &["hunter2"],
        ),
        (
            "postgresql://db.example.com/db?sslpassword=hunter2",
            &["hunter2"],
        ),
        (
            "postgresql://db.example.com/db?oauth_client_secret=hunter2",
            &["hunter2"],
        ),
        (
            "postgresql://db.example.com/db?scram_client_key=hunter2&scram_server_key=hunter3",
            &["hunter2", "hunter3"],
        ),
        ("host=h password=hun;ter&2'x", &["hun", "ter", "2'x"]),
        (
            "postgresql://db.example.com/db?sslpassword=alpha;bravo",
            &["alpha", "bravo"],
        ),
        (
            "postgresql://db.example.com/db?sslpassword=alpha&bravo",
            &["alpha", "bravo"],
        ),
        (
            "postgresql://db.example.com/db?password=***;rest",
            &["rest"],
        ),
        (
            "postgresql://db.example.com/db?password=***&rest",
            &["rest"],
        ),
        ("host=h password=***;rest", &["rest"]),
        ("host=h password=***&rest", &["rest"]),
        ("host=h pasword=hunter2", &["hunter2"]),
        (
            "postgresql://db.example.com/db?sslmode=require&pasword=hunter2",
            &["hunter2"],
        ),
    ] {
        let message = uri_error_message(input);
        for secret in secrets {
            assert!(!message.contains(secret), "{secret:?} in {message}");
        }
    }
}

#[test]
fn public_keys_stay_readable_in_the_error() {
    assert_eq!(
        uri_error_message("host=h port=5432 sslmode=require sslkey=k.pem passfile=pgpass"),
        "Invalid connection URI format: host=h port=5432 sslmode=require sslkey=k.pem \
         passfile=pgpass"
    );
    assert_eq!(
        uri_error_message("postgresql://db.example.com/db?sslmode=require&connect_timeout=5"),
        "Invalid connection URI format: postgresql://db.example.com/db?sslmode=***"
    );
}

#[test]
fn userinfo_password_with_value_characters_is_hidden_in_the_error() {
    for input in [
        "postgresql://u:alpha&password=x@db.example.com/db",
        "postgresql://u:alpha;bravo@db.example.com/db",
    ] {
        let message = uri_error_message(input);
        for secret in ["alpha", "bravo", "x@"] {
            assert!(!message.contains(secret), "{secret:?} in {message}");
        }
        assert!(message.contains("u:***@db.example.com/db"), "{message}");
    }
    assert_eq!(
        uri_error_message("postgresql://u:secret@db.example.com/db"),
        "Invalid connection URI format: postgresql://u:***@db.example.com/db"
    );
}

#[test]
fn options_value_is_hidden_in_the_error() {
    for input in [
        "postgresql://db.example.com/db?options=-c%20app.api_token%3Dhunter2",
        "host=h options='-c app.api_token=hunter2'",
    ] {
        let message = uri_error_message(input);
        assert!(!message.contains(SECRET), "{message}");
    }
}

#[test]
fn at_sign_in_a_query_value_hides_the_whole_tail_in_the_error() {
    for input in [
        "postgresql://u:p@db.example.com/db?password=alpha@bravo.example.com",
        "postgresql://u:p@db.example.com/db?options=-c%20app.api_token%3Dalpha@bravo.example.com",
    ] {
        let message = uri_error_message(input);
        for secret in ["alpha", "bravo", "u:p"] {
            assert!(!message.contains(secret), "{secret:?} in {message}");
        }
    }
}

#[test]
fn nested_quotes_in_a_public_value_hide_the_secret() {
    for input in [
        r"host=h application_name='password=\'alpha bravo\'' port=5432",
        r"host=h application_name='x pasword=\'alpha bravo\' y'",
    ] {
        let shown = sanitize_connection_string(input);
        let message = uri_error_message(input);
        for text in [&shown, &message] {
            assert!(!text.contains("alpha"), "{text}");
            assert!(!text.contains("bravo"), "{text}");
        }
        assert!(shown.contains("host=h"), "{shown}");
    }
}

#[test]
fn non_ascii_keys_hide_their_value() {
    for input in [
        "postgresql://u@db.example.com/db?密码=hunter2",
        "postgresql://u@db.example.com/db?密码=alpha@bravo.example.com",
        "host=h 密码=hunter2 port=5432",
    ] {
        let shown = sanitize_connection_string(input);
        let message = uri_error_message(input);
        for text in [&shown, &message] {
            for secret in [SECRET, "alpha", "bravo"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
    }
}

#[test]
fn any_key_that_is_not_public_hides_its_value() {
    for input in [
        "postgresql://db.example.com/db?🔑=hunter2",
        "postgresql://db.example.com/db?x\u{301}=hunter2",
        "postgresql://db.example.com/db?x-host=hunter2",
        "postgresql://db.example.com/db?-=hunter2",
        "postgresql://db.example.com/db?host-port=hunter2",
        "postgresql://u@db.example.com/db?🔑=alpha@bravo.example.com",
        "host=h application_name=password='alpha'bravo",
    ] {
        let shown = sanitize_connection_string(input);
        let message = uri_error_message(input);
        for text in [&shown, &message] {
            for secret in [SECRET, "alpha", "bravo"] {
                assert!(!text.contains(secret), "{input}: {text}");
            }
        }
    }
}

#[test]
fn an_escaped_final_apostrophe_of_an_unclosed_value_is_hidden() {
    for (input, expected) in [
        (
            "postgresql://db.example.com/db?x='alpha\\'",
            "postgresql://db.example.com/db?x='***",
        ),
        (
            "host=db.example.com x='alpha\\'",
            "[connection string hidden]",
        ),
    ] {
        assert_eq!(sanitize_connection_string(input), expected);
    }
    assert!(
        sanitize_connection_string("postgresql://db.example.com/db?x='alpha'").ends_with("x='***'")
    );
}

#[test]
fn a_plain_keyword_string_stays_readable() {
    let readable = "host=h port=5432 sslmode=require";
    assert_eq!(sanitize_connection_string(readable), readable);
}
