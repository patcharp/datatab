#[test]
fn timeout_does_not_take_over_socket() {
    let error = std::io::Error::new(std::io::ErrorKind::TimedOut, "not responding");
    assert!(super::focus_answered(Err(error)).is_err());
}
