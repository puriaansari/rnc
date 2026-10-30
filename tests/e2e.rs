use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

// Network tests are serialized so that process startup and port reuse are
// deterministic on Windows.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn rnc(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rnc"));
    command.args(args);
    command
}

fn wait_for_output(mut child: Child, timeout: Duration) -> Output {
    let started = Instant::now();

    loop {
        match child.try_wait().expect("could not poll child") {
            Some(_) => {
                return child
                    .wait_with_output()
                    .expect("could not collect child output");
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let output = child
                    .wait_with_output()
                    .expect("could not collect timed-out child output");
                panic!(
                    "rnc timed out\nstdout: {:?}\nstderr: {:?}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn free_tcp_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("could not reserve a port");
    listener
        .local_addr()
        .expect("could not read local address")
        .port()
}

fn free_udp_port() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("could not reserve a UDP port");
    socket
        .local_addr()
        .expect("could not read local address")
        .port()
}

fn wait_for_udp_listener(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        match UdpSocket::bind(("127.0.0.1", port)) {
            Ok(probe) => {
                drop(probe);
                if Instant::now() >= deadline {
                    panic!("UDP listener did not bind port {port}");
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return,
        }
    }
}

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return stream,
            Err(error) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
                let _ = error;
            }
            Err(error) => panic!("could not connect to test listener on port {port}: {error}"),
        }
    }
}

#[test]
fn tcp_client_sends_and_receives_data() {
    let _guard = serial();
    let listener = TcpListener::bind("127.0.0.1:0").expect("could not bind test server");
    let port = listener.local_addr().unwrap().port();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("test server did not get a connection");
        let mut request = [0_u8; 4];
        stream
            .read_exact(&mut request)
            .expect("test server read failed");
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").expect("test server write failed");
        stream
            .shutdown(Shutdown::Write)
            .expect("test server shutdown failed");
    });

    let mut child = rnc(&["-N", "-w", "5", "127.0.0.1", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start rnc client");

    child
        .stdin
        .take()
        .expect("client stdin was not piped")
        .write_all(b"ping")
        .expect("could not write to client stdin");

    let output = wait_for_output(child, Duration::from_secs(5));
    server.join().expect("test server panicked");

    assert!(
        output.status.success(),
        "client failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pong");
}

#[test]
fn tcp_listener_receives_data() {
    let _guard = serial();
    let port = free_tcp_port();
    let child = rnc(&["-l", "-w", "5", "127.0.0.1", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start rnc listener");

    let mut stream = connect_with_retry(port);
    stream
        .write_all(b"hello")
        .expect("could not write to listener");
    stream
        .shutdown(Shutdown::Write)
        .expect("could not half-close client");

    let output = wait_for_output(child, Duration::from_secs(5));

    assert!(
        output.status.success(),
        "listener failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello");
}

#[test]
fn udp_client_sends_and_receives_datagrams() {
    let _guard = serial();
    let socket = UdpSocket::bind("127.0.0.1:0").expect("could not bind UDP test server");
    let port = socket.local_addr().unwrap().port();

    let server = thread::spawn(move || {
        let mut request = [0_u8; 4];
        let (read, peer) = socket
            .recv_from(&mut request)
            .expect("UDP test server read failed");
        assert_eq!(&request[..read], b"ping");
        socket
            .send_to(b"pong", peer)
            .expect("UDP test server write failed");
    });

    let mut child = rnc(&["-u", "-w", "0.3", "127.0.0.1", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start rnc UDP client");

    child
        .stdin
        .take()
        .expect("client stdin was not piped")
        .write_all(b"ping")
        .expect("could not write to client stdin");

    let output = wait_for_output(child, Duration::from_secs(5));
    server.join().expect("UDP test server panicked");

    assert!(
        output.status.success(),
        "UDP client failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pong");
}

#[test]
fn tcp_listener_keep_open_uses_stdin_for_later_connections() {
    let _guard = serial();
    let port = free_tcp_port();
    let mut child = rnc(&["-l", "-k", "-w", "2", "127.0.0.1", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start keep-open listener");

    let mut first = connect_with_retry(port);
    first
        .write_all(b"first")
        .expect("could not write first payload");
    first
        .shutdown(Shutdown::Write)
        .expect("could not close first connection");
    drop(first);

    // Let the listener finish the first connection and return to accept().
    thread::sleep(Duration::from_millis(100));

    let mut second = connect_with_retry(port);
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("could not set client read timeout");

    let stdin = child.stdin.as_mut().expect("listener stdin was not piped");
    stdin
        .write_all(b"hello")
        .expect("could not write to listener stdin");
    stdin.flush().expect("could not flush listener stdin");

    let mut reply = [0_u8; 5];
    second
        .read_exact(&mut reply)
        .expect("second connection did not receive stdin data");
    assert_eq!(&reply, b"hello");

    second
        .shutdown(Shutdown::Write)
        .expect("could not close second connection");

    child.kill().expect("could not stop keep-open listener");
    let output = child
        .wait_with_output()
        .expect("could not collect keep-open listener output");

    assert_eq!(output.stdout, b"first");
    assert!(
        output.stderr.is_empty(),
        "keep-open listener wrote to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn udp_listener_prints_datagrams_and_replies_to_the_sender() {
    let _guard = serial();
    let port = free_udp_port();
    let mut child = rnc(&["-u", "-l", "127.0.0.1", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start UDP listener");

    wait_for_udp_listener(port);

    let socket = UdpSocket::bind("127.0.0.1:0").expect("could not bind UDP client");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("could not set UDP read timeout");
    socket
        .send_to(b"hello", ("127.0.0.1", port))
        .expect("could not send to UDP listener");

    // The listener learns the sender from the datagram, then routes stdin back
    // to that peer.
    thread::sleep(Duration::from_millis(100));
    let stdin = child.stdin.as_mut().expect("listener stdin was not piped");
    stdin
        .write_all(b"world")
        .expect("could not write to UDP listener stdin");
    stdin.flush().expect("could not flush listener stdin");

    let mut reply = [0_u8; 5];
    let (read, _) = socket
        .recv_from(&mut reply)
        .expect("UDP listener did not send a reply");
    assert_eq!(&reply[..read], b"world");

    child.kill().expect("could not stop UDP listener");
    let output = child
        .wait_with_output()
        .expect("could not collect UDP listener output");

    assert_eq!(output.stdout, b"hello");
    assert!(
        output.stderr.is_empty(),
        "UDP listener wrote to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
