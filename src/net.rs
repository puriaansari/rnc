use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol as SocketProtocol, Socket, Type};

use crate::cli::{Config, IpVersion, Protocol};

type BoxError = Box<dyn Error>;

pub fn run(config: Config) -> Result<(), BoxError> {
    match (config.listen, config.protocol) {
        (false, Protocol::Tcp) => run_tcp_connect(config),
        (true, Protocol::Tcp) => run_tcp_listen(config),
        (false, Protocol::Udp) => run_udp_connect(config),
        (true, Protocol::Udp) => run_udp_listen(config),
    }
}

fn run_tcp_connect(config: Config) -> Result<(), BoxError> {
    let host = config
        .host
        .as_deref()
        .ok_or_else(|| invalid_input("connect mode needs a host"))?;
    let addresses = resolve_host(host, config.port, &config)?;
    let mut last_error = None;

    for address in addresses {
        match connect_tcp(address, &config) {
            Ok(stream) => {
                if config.verbose {
                    eprintln!("rnc: connected to {address}");
                }

                if config.zero_io {
                    if config.verbose {
                        eprintln!("rnc: {address} is open");
                    }
                    return Ok(());
                }

                relay_tcp(stream, &config)?;
                return Ok(());
            }
            Err(error) => {
                if config.verbose {
                    eprintln!("rnc: connect to {address} failed: {error}");
                }
                last_error = Some(error);
            }
        }
    }

    let detail = last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "no usable address".to_owned());

    Err(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!("could not connect to {host}:{}: {detail}", config.port),
    )
    .into())
}

fn connect_tcp(address: SocketAddr, config: &Config) -> io::Result<TcpStream> {
    let domain = if address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(SocketProtocol::TCP))?;

    if config.source.is_some() || config.local_port.is_some() {
        let source_ip = source_ip(config, &address)?;
        let source = SocketAddr::new(source_ip, config.local_port.unwrap_or(0));
        socket.set_reuse_address(true)?;
        socket.bind(&source.into()).map_err(|error| {
            contextual(error, format!("could not bind source address {source}"))
        })?;
    }

    let result = match config.timeout {
        Some(timeout) => socket.connect_timeout(&address.into(), timeout),
        None => socket.connect(&address.into()),
    };

    result.map_err(|error| contextual(error, format!("connect to {address} failed")))?;

    let stream: TcpStream = socket.into();
    configure_tcp_stream(&stream, config)?;
    Ok(stream)
}

struct ListenerInput {
    state: Mutex<ListenerInputState>,
    ready: Condvar,
}

struct ListenerInputState {
    writer: Option<TcpStream>,
    eof: bool,
}

impl ListenerInput {
    fn new() -> Self {
        Self {
            state: Mutex::new(ListenerInputState {
                writer: None,
                eof: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ListenerInputState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn run_tcp_listen(config: Config) -> Result<(), BoxError> {
    let addresses = listen_addresses(&config)?;
    let listener = bind_tcp_listener(&addresses)?;
    let local = listener.local_addr()?;

    if config.verbose || config.port == 0 {
        eprintln!("rnc: listening on {local} (tcp)");
    }

    let input = Arc::new(ListenerInput::new());
    spawn_listener_stdin(Arc::clone(&input), &config)?;

    loop {
        let (stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(contextual(error, "accept failed").into()),
        };

        if config.verbose {
            eprintln!("rnc: connection from {peer}");
        }

        let result = configure_tcp_stream(&stream, &config)
            .and_then(|()| set_listener_writer(&input, &stream, &config))
            .and_then(|()| drain_tcp(stream, &config));

        clear_listener_writer(&input);

        if let Err(error) = result {
            if config.keep_open {
                eprintln!("rnc: connection from {peer} ended with an error: {error}");
            } else {
                return Err(error.into());
            }
        }

        if !config.keep_open {
            break;
        }
    }

    Ok(())
}

fn spawn_listener_stdin(input: Arc<ListenerInput>, config: &Config) -> io::Result<()> {
    let verbose = config.verbose;
    let shutdown_on_eof = config.shutdown_on_eof;

    thread::Builder::new()
        .name("rnc-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = vec![0_u8; 8_192];

            loop {
                let read = match stdin.read(&mut buffer) {
                    Ok(0) => {
                        let mut state = input.lock();
                        state.eof = true;

                        if shutdown_on_eof
                            && let Some(writer) = state.writer.as_ref()
                            && let Err(error) = writer.shutdown(Shutdown::Write)
                            && verbose
                        {
                            eprintln!("rnc: could not shut down the write side: {error}");
                        }

                        input.ready.notify_all();
                        break;
                    }
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        if verbose {
                            eprintln!("rnc: could not read stdin: {error}");
                        }
                        break;
                    }
                };

                let mut state = input.lock();

                while state.writer.is_none() && !state.eof {
                    state = input
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }

                if state.eof {
                    break;
                }

                let write_result = match state.writer.as_mut() {
                    Some(writer) => writer
                        .write_all(&buffer[..read])
                        .and_then(|()| writer.flush()),
                    None => continue,
                };

                if let Err(error) = write_result {
                    if verbose {
                        eprintln!("rnc: TCP send failed: {error}");
                    }
                    state.writer = None;
                    input.ready.notify_all();
                }
            }
        })
        .map_err(|error| contextual(error, "could not start the stdin relay thread"))?;

    Ok(())
}

fn set_listener_writer(
    input: &ListenerInput,
    stream: &TcpStream,
    config: &Config,
) -> io::Result<()> {
    let writer = stream.try_clone()?;
    let mut state = input.lock();
    state.writer = Some(writer);

    if state.eof && config.shutdown_on_eof {
        stream.shutdown(Shutdown::Write)?;
    }

    input.ready.notify_all();
    Ok(())
}

fn clear_listener_writer(input: &ListenerInput) {
    let mut state = input.lock();
    state.writer = None;
    input.ready.notify_all();
}

fn bind_tcp_listener(addresses: &[SocketAddr]) -> io::Result<TcpListener> {
    let mut last_error = None;

    for address in addresses {
        match TcpListener::bind(address) {
            Ok(listener) => return Ok(listener),
            Err(error) => {
                last_error = Some(contextual(error, format!("could not listen on {address}")));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| other("no address available to listen on")))
}

fn configure_tcp_stream(stream: &TcpStream, config: &Config) -> io::Result<()> {
    stream.set_nodelay(true)?;

    if let Some(timeout) = config.timeout {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| contextual(error, "could not set read timeout"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| contextual(error, "could not set write timeout"))?;
    }

    Ok(())
}

fn relay_tcp(reader: TcpStream, config: &Config) -> io::Result<()> {
    let mut writer = reader.try_clone()?;
    let verbose = config.verbose;
    let shutdown_on_eof = config.shutdown_on_eof;

    thread::Builder::new()
        .name("rnc-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let result = io::copy(&mut stdin, &mut writer);

            if let Err(error) = &result
                && verbose
            {
                eprintln!("rnc: stdin relay stopped: {error}");
            }

            let _ = writer.flush();

            if result.is_ok()
                && shutdown_on_eof
                && let Err(error) = writer.shutdown(Shutdown::Write)
                && verbose
            {
                eprintln!("rnc: could not shut down the write side: {error}");
            }
        })
        .map_err(|error| contextual(error, "could not start the stdin relay thread"))?;

    drain_tcp(reader, config)
}

fn drain_tcp(mut reader: TcpStream, config: &Config) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let result = io::copy(&mut reader, &mut stdout);
    stdout.flush()?;

    match result {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ) =>
        {
            Ok(())
        }
        Err(error) if is_timeout(&error) => Err(contextual(
            error,
            format!("timed out after {}s", timeout_seconds(config)),
        )),
        Err(error) => Err(error),
    }
}

fn run_udp_connect(config: Config) -> Result<(), BoxError> {
    let host = config
        .host
        .as_deref()
        .ok_or_else(|| invalid_input("connect mode needs a host"))?;
    let addresses = resolve_host(host, config.port, &config)?;
    let address = addresses[0];
    let source_ip = source_ip(&config, &address)?;
    let local = SocketAddr::new(source_ip, config.local_port.unwrap_or(0));

    let socket = UdpSocket::bind(local)
        .map_err(|error| contextual(error, format!("could not bind source address {local}")))?;
    socket
        .connect(address)
        .map_err(|error| contextual(error, format!("could not connect to {address}")))?;

    if config.verbose {
        eprintln!("rnc: connected to {address} (udp)");
    }

    relay_udp_connected(&socket, &config)?;
    Ok(())
}

fn relay_udp_connected(socket: &UdpSocket, config: &Config) -> io::Result<()> {
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    let (sender, events) = mpsc::channel();
    let sender_socket = socket.try_clone()?;
    let verbose = config.verbose;

    thread::Builder::new()
        .name("rnc-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = vec![0_u8; 65_507];

            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Ok(()));
                        break;
                    }
                    Ok(read) => {
                        if let Err(error) = sender_socket.send(&buffer[..read]) {
                            if verbose {
                                eprintln!("rnc: UDP send failed: {error}");
                            }
                            let _ = sender.send(Err(format!("UDP send failed: {error}")));
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        if verbose {
                            eprintln!("rnc: could not read stdin: {error}");
                        }
                        let _ = sender.send(Err(format!("could not read stdin: {error}")));
                        break;
                    }
                }
            }
        })
        .map_err(|error| contextual(error, "could not start the stdin relay thread"))?;

    // UDP has no half-close, so after stdin ends wait for a final response.
    // A response resets the timer; `-w` controls how long to linger.
    let linger = config.timeout.unwrap_or_else(|| Duration::from_secs(1));
    let mut eof_at = None;
    let mut last_receive = Instant::now();
    let mut stdout = io::stdout().lock();
    let mut buffer = vec![0_u8; 65_535];

    loop {
        match events.try_recv() {
            Ok(Ok(())) => {
                eof_at.get_or_insert_with(Instant::now);
            }
            Ok(Err(message)) => return Err(other(message)),
            Err(TryRecvError::Disconnected) => {
                eof_at.get_or_insert_with(Instant::now);
            }
            Err(TryRecvError::Empty) => {}
        }

        match socket.recv(&mut buffer) {
            Ok(read) => {
                stdout.write_all(&buffer[..read])?;
                stdout.flush()?;
                last_receive = Instant::now();
            }
            Err(error) if is_timeout(&error) || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(contextual(error, "UDP receive failed")),
        }

        if let Some(eof_at) = eof_at {
            let quiet_since = if last_receive > eof_at {
                last_receive
            } else {
                eof_at
            };

            if quiet_since.elapsed() >= linger {
                break;
            }
        }
    }

    stdout.flush()?;
    Ok(())
}

fn run_udp_listen(config: Config) -> Result<(), BoxError> {
    let addresses = listen_addresses(&config)?;
    let socket = bind_udp_socket(&addresses)?;
    let local = socket.local_addr()?;

    if config.verbose || config.port == 0 {
        eprintln!("rnc: listening on {local} (udp)");
    }

    let last_peer = Arc::new((Mutex::new(None::<SocketAddr>), Condvar::new()));
    let sender_socket = socket.try_clone()?;
    let sender_peer = Arc::clone(&last_peer);
    let verbose = config.verbose;

    thread::Builder::new()
        .name("rnc-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = vec![0_u8; 65_507];

            loop {
                let read = match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        if verbose {
                            eprintln!("rnc: could not read stdin: {error}");
                        }
                        break;
                    }
                };

                let peer = {
                    let (lock, ready) = &*sender_peer;
                    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

                    while guard.is_none() {
                        guard = ready
                            .wait(guard)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }

                    *guard
                };

                if let Some(peer) = peer
                    && let Err(error) = sender_socket.send_to(&buffer[..read], peer)
                {
                    if verbose {
                        eprintln!("rnc: UDP send to {peer} failed: {error}");
                    }
                    break;
                }
            }
        })
        .map_err(|error| contextual(error, "could not start the stdin relay thread"))?;

    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut stdout = io::stdout().lock();
    let mut buffer = vec![0_u8; 65_535];

    loop {
        match socket.recv_from(&mut buffer) {
            Ok((read, peer)) => {
                {
                    let (lock, ready) = &*last_peer;
                    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    *guard = Some(peer);
                    ready.notify_all();
                }

                if config.verbose {
                    eprintln!("rnc: datagram from {peer}");
                }

                stdout.write_all(&buffer[..read])?;
                stdout.flush()?;
            }
            Err(error) if is_timeout(&error) || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(contextual(error, "UDP receive failed").into()),
        }
    }
}

fn bind_udp_socket(addresses: &[SocketAddr]) -> io::Result<UdpSocket> {
    let mut last_error = None;

    for address in addresses {
        match UdpSocket::bind(address) {
            Ok(socket) => return Ok(socket),
            Err(error) => {
                last_error = Some(contextual(error, format!("could not listen on {address}")));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| other("no address available to listen on")))
}

fn resolve_host(host: &str, port: u16, config: &Config) -> io::Result<Vec<SocketAddr>> {
    let addresses: Vec<SocketAddr> = if config.numeric {
        let ip = host.parse::<IpAddr>().map_err(|_| {
            invalid_input(format!(
                "'{host}' is not a numeric IP address (remove -n to use DNS)"
            ))
        })?;
        vec![SocketAddr::new(ip, port)]
    } else {
        (host, port)
            .to_socket_addrs()
            .map_err(|error| contextual(error, format!("could not resolve '{host}'")))?
            .collect()
    };

    let filtered: Vec<SocketAddr> = addresses
        .into_iter()
        .filter(|address| family_matches(*address, config.family))
        .collect();

    if filtered.is_empty() {
        return Err(other(format!(
            "no {} address found for '{host}'",
            family_label(config.family)
        )));
    }

    Ok(filtered)
}

fn listen_addresses(config: &Config) -> io::Result<Vec<SocketAddr>> {
    match config.host.as_deref() {
        Some(host) => resolve_host(host, config.port, config),
        None => {
            let ip = match config.family {
                Some(IpVersion::V6) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            };
            Ok(vec![SocketAddr::new(ip, config.port)])
        }
    }
}

fn source_ip(config: &Config, remote: &SocketAddr) -> io::Result<IpAddr> {
    match config.source {
        Some(source) => {
            if source.is_ipv4() != remote.is_ipv4() {
                return Err(invalid_input(format!(
                    "source address {source} and destination {remote} use different IP versions"
                )));
            }
            Ok(source)
        }
        None if remote.is_ipv4() => Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        None => Ok(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
    }
}

fn family_matches(address: SocketAddr, family: Option<IpVersion>) -> bool {
    match family {
        Some(IpVersion::V4) => address.is_ipv4(),
        Some(IpVersion::V6) => address.is_ipv6(),
        None => true,
    }
}

fn family_label(family: Option<IpVersion>) -> &'static str {
    match family {
        Some(IpVersion::V4) => "IPv4",
        Some(IpVersion::V6) => "IPv6",
        None => "IP",
    }
}

fn timeout_seconds(config: &Config) -> f64 {
    config
        .timeout
        .map(|timeout| timeout.as_secs_f64())
        .unwrap_or(0.0)
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn contextual(error: io::Error, context: impl std::fmt::Display) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
