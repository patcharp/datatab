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
fn unreadable_keyword_string_is_hidden_whole() {
    for input in ["host=h password='hunter 2", "host=h password=hunter 2"] {
        assert_eq!(
            uri_error_message(input),
            "Invalid connection URI format: [connection string hidden]"
        );
    }
}

#[test]
fn url_connect_error_still_redacts_credentials() {
    for (input, expected) in [
        (
            "postgresql://localhost:5432/app?password=hunter2&sslmode=require",
            "postgresql://localhost:5432/app?password=***&sslmode=require",
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a local port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);

    let message = connect_error_message(&format!(
        "host=127.0.0.1 port={port} user=app password=hunter2 sslmode=disable connect_timeout=5"
    ));
    assert!(!message.contains(SECRET), "{message}");
    let shown = format!(
        "Connection error with URI host=127.0.0.1 port={port} user=app password=*** \
         sslmode=disable connect_timeout=5: "
    );
    assert!(message.starts_with(&shown), "{message}");
}
