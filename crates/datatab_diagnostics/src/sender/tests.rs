use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use uuid::Uuid;

use super::*;
use crate::test_http::serve_once;

const REPORT_ID: &str = "6f1c6c2e-6d1e-4c50-9b34-91a1a2b3c4d5";

fn enabled() -> ReportSwitch {
    ReportSwitch::new(true, None, false)
}

fn disabled_by_setting() -> ReportSwitch {
    ReportSwitch::new(false, None, false)
}

#[test]
fn a_non_https_endpoint_is_refused() {
    let error = ReportSender::at("http://127.0.0.1:9").expect_err("http is refused");

    assert!(matches!(error, SendError::InsecureEndpoint(_)));
}

#[test]
fn send_posts_the_json_body_and_reads_the_delete_token() {
    let answer = format!(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-123\"}}",
        format!("{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-123\"}}").len()
    );
    let (addr, server) = serve_once(Box::leak(answer.into_boxed_str()));

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let body = br#"{"schema":1,"kind":"panic"}"#;

    let submitted = sender.send(body).expect("send succeeds");
    let request = server.join().expect("server thread");

    assert_eq!(submitted.report_id, Uuid::parse_str(REPORT_ID).unwrap());
    assert_eq!(submitted.delete_token, "tok-123");

    assert!(request.starts_with("POST /v1/reports HTTP/1.1\r\n"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("user-agent: datatab/"),
        "a fixed user agent must be sent: {request}"
    );
    assert!(request.ends_with(std::str::from_utf8(body).unwrap()));
}

#[test]
fn delete_carries_the_per_report_token() {
    let (addr, server) =
        serve_once("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let report_id = Uuid::parse_str(REPORT_ID).unwrap();

    sender
        .delete(report_id, "tok-456")
        .expect("delete succeeds");
    let request = server.join().expect("server thread");

    assert!(request.starts_with(&format!("DELETE /v1/reports/{REPORT_ID} HTTP/1.1\r\n")));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer tok-456"),
        "the delete token must ride in the request: {request}"
    );
}

/// The delete token is a credential: a log line or a failed assertion that
/// prints a submit must not print it.
#[test]
fn the_debug_output_hides_the_delete_token() {
    let submitted = SubmittedReport {
        report_id: Uuid::parse_str(REPORT_ID).unwrap(),
        delete_token: "tok-secret".to_string(),
    };

    let direct = format!("{submitted:?}");
    let nested = format!("{:?}", SubmitOutcome::Sent(submitted));

    for printed in [direct, nested] {
        assert!(!printed.contains("tok-secret"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }
}

#[test]
fn a_body_over_the_cap_is_refused_before_any_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let addr = listener.local_addr().expect("local addr");

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let oversized = vec![b'x'; crate::report::MAX_REPORT_BYTES + 1];

    let error = sender.send(&oversized).expect_err("refused");

    assert!(matches!(error, SendError::TooLarge { .. }));
    assert!(
        listener.accept().is_err(),
        "no socket may be opened for a refused body"
    );
}

#[test]
fn a_disabled_switch_opens_no_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let addr = listener.local_addr().expect("local addr");

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let client = ReportClient::with_sender(sender);

    let outcome = client
        .submit(&disabled_by_setting(), br#"{"schema":1}"#)
        .expect("submit returns");

    assert_eq!(outcome, SubmitOutcome::Disabled(DisableSource::Setting));
    thread::sleep(Duration::from_millis(100));
    assert!(
        listener.accept().is_err(),
        "a disabled switch must not open a socket"
    );
}

#[test]
fn a_switch_flip_applies_without_a_new_client() {
    let answer = format!(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-1\"}}",
        format!("{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-1\"}}").len()
    );
    let (addr, server) = serve_once(Box::leak(answer.into_boxed_str()));

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let client = ReportClient::with_sender(sender);

    let first = client
        .submit(&enabled(), br#"{"schema":1}"#)
        .expect("the first submit runs");
    assert!(matches!(first, SubmitOutcome::Sent(_)));
    server.join().expect("server thread");

    // The same client, a switch the user just turned off: it must not open a
    // connection, and a snapshot taken at construction would have tried to.
    let second = client
        .submit(&disabled_by_setting(), br#"{"schema":1}"#)
        .expect("the second submit returns");

    assert_eq!(second, SubmitOutcome::Disabled(DisableSource::Setting));

    let third = client
        .submit(
            &ReportSwitch::new(true, Some("1".to_string()), false),
            br#"{"schema":1}"#,
        )
        .expect("the third submit returns");

    assert_eq!(third, SubmitOutcome::Disabled(DisableSource::Environment));
}

#[test]
fn an_unused_client_opens_no_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let addr = listener.local_addr().expect("local addr");

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let _client = ReportClient::with_sender(sender);

    thread::sleep(Duration::from_millis(100));
    assert!(
        listener.accept().is_err(),
        "building a client must not contact the service"
    );
}

#[test]
fn an_enabled_switch_sends_through_the_client() {
    let answer = format!(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-789\"}}",
        format!("{{\"report_id\":\"{REPORT_ID}\",\"delete_token\":\"tok-789\"}}").len()
    );
    let (addr, server) = serve_once(Box::leak(answer.into_boxed_str()));

    let sender = ReportSender::for_test(&format!("http://{addr}")).expect("test sender");
    let client = ReportClient::with_sender(sender);

    let outcome = client.submit(&enabled(), br#"{"schema":1}"#).expect("sent");
    server.join().expect("server thread");

    match outcome {
        SubmitOutcome::Sent(submitted) => assert_eq!(submitted.delete_token, "tok-789"),
        other => panic!("expected Sent, got {other:?}"),
    }
}
