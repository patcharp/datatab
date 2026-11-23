use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Recording = Arc<Mutex<Vec<Vec<u8>>>>;

pub(crate) struct RecordingProxy {
    pub(crate) port: u16,
    recording: Recording,
    stop: Arc<AtomicBool>,
    sockets: Arc<Mutex<Vec<TcpStream>>>,
    listener: Option<JoinHandle<Vec<JoinHandle<()>>>>,
}

impl RecordingProxy {
    pub(crate) fn start(upstream: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind recording proxy");
        let port = listener.local_addr().expect("proxy address").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let recording = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let handle = thread::spawn({
            let recording = recording.clone();
            let stop = stop.clone();
            let sockets = sockets.clone();
            move || {
                let mut workers = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            if let Ok(server) = TcpStream::connect(("127.0.0.1", upstream)) {
                                let index = {
                                    let mut records = recording.lock().expect("recording lock");
                                    records.push(Vec::new());
                                    records.len() - 1
                                };
                                let reverse_client = client.try_clone().expect("clone client");
                                let reverse_server = server.try_clone().expect("clone server");
                                sockets.lock().expect("socket lock").extend([
                                    client.try_clone().expect("track client"),
                                    server.try_clone().expect("track server"),
                                ]);
                                workers.push(thread::spawn({
                                    let recording = recording.clone();
                                    move || relay(client, server, Some((recording, index)))
                                }));
                                workers.push(thread::spawn(move || {
                                    relay(reverse_server, reverse_client, None)
                                }));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("proxy accept failed: {error}"),
                    }
                }
                workers
            }
        });
        Self {
            port,
            recording,
            stop,
            sockets,
            listener: Some(handle),
        }
    }

    pub(crate) fn assert_tls_only(&self, markers: &[&str]) {
        let records = self.recording.lock().expect("recording lock");
        assert!(!records.is_empty(), "proxy must observe a connection");
        for bytes in records.iter().filter(|bytes| !bytes.is_empty()) {
            assert_eq!(bytes.first(), Some(&0x16), "connection must start with TLS");
            for marker in markers {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "application text crossed the relay in clear"
                );
            }
        }
    }
}

fn relay(mut source: TcpStream, mut target: TcpStream, recording: Option<(Recording, usize)>) {
    let mut buffer = [0_u8; 4096];
    while let Ok(count) = source.read(&mut buffer) {
        if count == 0 {
            break;
        }
        if let Some((records, index)) = &recording {
            records.lock().expect("recording lock")[*index].extend_from_slice(&buffer[..count]);
        }
        if target.write_all(&buffer[..count]).is_err() {
            break;
        }
    }
    if let Err(error) = target.shutdown(Shutdown::Write) {
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::NotConnected | std::io::ErrorKind::BrokenPipe
            ),
            "proxy shutdown: {error}"
        );
    }
}

impl Drop for RecordingProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let workers = self
            .listener
            .take()
            .expect("proxy listener")
            .join()
            .expect("join listener");
        for socket in self.sockets.lock().expect("socket lock").iter() {
            if let Err(error) = socket.shutdown(Shutdown::Both) {
                assert!(
                    error.kind() == std::io::ErrorKind::NotConnected,
                    "close proxy socket: {error}"
                );
            }
        }
        for worker in workers {
            worker.join().expect("join relay");
        }
    }
}
