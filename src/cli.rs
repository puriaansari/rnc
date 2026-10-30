use std::net::IpAddr;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "\
Usage: rnc [OPTIONS] <host> <port>
       rnc [OPTIONS] -l [host] <port>
       rnc [OPTIONS] -l -p <port> [host]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    V4,
    V6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: bool,
    pub protocol: Protocol,
    pub family: Option<IpVersion>,
    pub verbose: bool,
    pub numeric: bool,
    pub zero_io: bool,
    pub keep_open: bool,
    pub shutdown_on_eof: bool,
    pub timeout: Option<Duration>,
    pub source: Option<IpAddr>,
    /// Source port in connect mode. In listen mode this is folded into `port`.
    pub local_port: Option<u16>,
    pub host: Option<String>,
    pub port: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Config),
    Help,
    Version,
}

pub fn help() -> String {
    format!(
        "rnc {VERSION}
A small netcat-compatible TCP/UDP client and server.

{USAGE}

Options:
  -4, --ipv4                 Use IPv4 only
  -6, --ipv6                 Use IPv6 only
  -u, --udp                  Use UDP instead of TCP
  -l, --listen               Listen for incoming connections
  -k, --keep-open            Keep listening after a connection closes
  -p, --local-port <PORT>    Local port (listen) or source port (connect)
  -s, --source <ADDR>        Local source IP address
  -w, --wait <SECONDS>       Connect/read timeout (fractions allowed)
  -N, --shutdown             Shut down the write side when stdin reaches EOF
  -z, --zero                 Zero-I/O TCP connect scan
  -n, --numeric              Do not resolve names; require a numeric IP
  -v, --verbose              Print connection details to stderr
  -h, --help                 Print this help
  -V, --version              Print version

Examples:
  rnc example.com 80
  printf 'GET / HTTP/1.0\\r\\n\\r\\n' | rnc example.com 80
  rnc -l 8080
  rnc -l -p 8080 -k
  rnc -u -l 5353
  rnc -zv -w 2 192.168.1.10 22
"
    )
}

pub fn parse_args<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter().map(|arg| arg.as_ref().to_owned());
    let mut listen = false;
    let mut udp = false;
    let mut family = None;
    let mut verbose = false;
    let mut numeric = false;
    let mut zero_io = false;
    let mut keep_open = false;
    let mut shutdown_on_eof = false;
    let mut timeout = None;
    let mut source = None;
    let mut local_port = None;
    let mut positionals = Vec::new();

    while let Some(arg) = iter.next() {
        if arg == "--" {
            positionals.extend(iter);
            break;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (body, None),
            };

            match name {
                "help" => {
                    reject_value(inline.as_deref(), "--help")?;
                    return Ok(Command::Help);
                }
                "version" => {
                    reject_value(inline.as_deref(), "--version")?;
                    return Ok(Command::Version);
                }
                "ipv4" => {
                    reject_value(inline.as_deref(), "--ipv4")?;
                    set_family(&mut family, IpVersion::V4)?;
                }
                "ipv6" => {
                    reject_value(inline.as_deref(), "--ipv6")?;
                    set_family(&mut family, IpVersion::V6)?;
                }
                "udp" => {
                    reject_value(inline.as_deref(), "--udp")?;
                    udp = true;
                }
                "listen" => {
                    reject_value(inline.as_deref(), "--listen")?;
                    listen = true;
                }
                "keep-open" => {
                    reject_value(inline.as_deref(), "--keep-open")?;
                    keep_open = true;
                }
                "shutdown" => {
                    reject_value(inline.as_deref(), "--shutdown")?;
                    shutdown_on_eof = true;
                }
                "zero" => {
                    reject_value(inline.as_deref(), "--zero")?;
                    zero_io = true;
                }
                "numeric" => {
                    reject_value(inline.as_deref(), "--numeric")?;
                    numeric = true;
                }
                "verbose" => {
                    reject_value(inline.as_deref(), "--verbose")?;
                    verbose = true;
                }
                "local-port" => {
                    let value = take_long_value(&mut iter, inline, "--local-port")?;
                    local_port = Some(parse_port(&value, true)?);
                }
                "source" => {
                    let value = take_long_value(&mut iter, inline, "--source")?;
                    source = Some(parse_source(&value)?);
                }
                "wait" => {
                    let value = take_long_value(&mut iter, inline, "--wait")?;
                    timeout = Some(parse_wait(&value)?);
                }
                _ => return Err(format!("unknown option '--{name}'")),
            }
            continue;
        }

        if arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg.chars().skip(1).collect();
            let mut index = 0;

            while index < chars.len() {
                let flag = chars[index];
                index += 1;

                match flag {
                    'h' => return Ok(Command::Help),
                    'V' => return Ok(Command::Version),
                    '4' => set_family(&mut family, IpVersion::V4)?,
                    '6' => set_family(&mut family, IpVersion::V6)?,
                    'u' => udp = true,
                    'l' => listen = true,
                    'k' => keep_open = true,
                    'N' => shutdown_on_eof = true,
                    'z' => zero_io = true,
                    'n' => numeric = true,
                    'v' => verbose = true,
                    'p' | 's' | 'w' => {
                        let rest: String = chars[index..].iter().collect();
                        index = chars.len();
                        let inline = if rest.is_empty() {
                            None
                        } else {
                            Some(rest.strip_prefix('=').unwrap_or(&rest).to_owned())
                        };
                        let value = take_short_value(&mut iter, inline, flag)?;
                        match flag {
                            'p' => local_port = Some(parse_port(&value, true)?),
                            's' => source = Some(parse_source(&value)?),
                            'w' => timeout = Some(parse_wait(&value)?),
                            _ => unreachable!(),
                        }
                    }
                    _ => return Err(format!("unknown option '-{flag}'")),
                }
            }
            continue;
        }

        positionals.push(arg);
    }

    if zero_io && listen {
        return Err("-z/--zero only works in connect mode".to_owned());
    }
    if zero_io && udp {
        return Err("-z/--zero only works with TCP".to_owned());
    }
    if keep_open && !listen {
        return Err("-k/--keep-open requires -l/--listen".to_owned());
    }
    if let (Some(source), Some(family)) = (source, family)
        && source.is_ipv4() != matches!(family, IpVersion::V4)
    {
        return Err(format!(
            "source address '{source}' does not match the selected IP version"
        ));
    }

    let (host, port, local_port) = if listen {
        match positionals.len() {
            0 => {
                let port = local_port.ok_or_else(|| {
                    "listen mode needs a port (for example: rnc -l 8080)".to_owned()
                })?;
                (None, port, None)
            }
            1 => {
                if let Some(port) = local_port {
                    (Some(positionals.remove(0)), port, None)
                } else {
                    let port = parse_port(&positionals[0], true)?;
                    (None, port, None)
                }
            }
            2 => {
                if local_port.is_some() {
                    return Err(
                        "specify the listen port either positionally or with -p, not both"
                            .to_owned(),
                    );
                }
                let port = parse_port(&positionals[1], true)?;
                (Some(positionals.remove(0)), port, None)
            }
            _ => {
                return Err("too many arguments; expected an optional host and one port".to_owned());
            }
        }
    } else {
        if positionals.len() != 2 {
            return Err("connect mode needs a host and a port".to_owned());
        }
        let port = parse_port(&positionals[1], false)?;
        (Some(positionals.remove(0)), port, local_port)
    };

    if host.as_deref().is_some_and(str::is_empty) {
        return Err("host must not be empty".to_owned());
    }

    Ok(Command::Run(Config {
        listen,
        protocol: if udp { Protocol::Udp } else { Protocol::Tcp },
        family,
        verbose,
        numeric,
        zero_io,
        keep_open,
        shutdown_on_eof,
        timeout,
        source,
        local_port,
        host,
        port,
    }))
}

fn reject_value(value: Option<&str>, option: &str) -> Result<(), String> {
    match value {
        Some(value) => Err(format!(
            "option '{option}' does not take a value (got '{value}')"
        )),
        None => Ok(()),
    }
}

fn set_family(family: &mut Option<IpVersion>, value: IpVersion) -> Result<(), String> {
    if let Some(existing) = *family
        && existing != value
    {
        return Err("-4/--ipv4 and -6/--ipv6 cannot be used together".to_owned());
    }
    *family = Some(value);
    Ok(())
}

fn take_long_value<I: Iterator<Item = String>>(
    iter: &mut I,
    inline: Option<String>,
    option: &str,
) -> Result<String, String> {
    match inline {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(format!("option '{option}' needs a value")),
        None => iter
            .next()
            .ok_or_else(|| format!("option '{option}' needs a value")),
    }
}

fn take_short_value<I: Iterator<Item = String>>(
    iter: &mut I,
    inline: Option<String>,
    flag: char,
) -> Result<String, String> {
    match inline {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(format!("option '-{flag}' needs a value")),
        None => iter
            .next()
            .ok_or_else(|| format!("option '-{flag}' needs a value")),
    }
}

fn parse_source(value: &str) -> Result<IpAddr, String> {
    value
        .parse::<IpAddr>()
        .map_err(|_| format!("invalid source address '{value}' (expected an IP address)"))
}

fn parse_port(value: &str, allow_zero: bool) -> Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| format!("invalid port '{value}' (expected 0-65535)"))?;

    if port == 0 && !allow_zero {
        return Err("port 0 is only valid when listening or as a source port".to_owned());
    }

    Ok(port)
}

fn parse_wait(value: &str) -> Result<Duration, String> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| format!("invalid timeout '{value}' (expected seconds)"))?;

    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("timeout must be a positive number of seconds".to_owned());
    }
    if seconds > 31_536_000.0 {
        return Err("timeout is too large (maximum is one year)".to_owned());
    }

    Ok(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(args: &[&str]) -> Config {
        match parse_args(args.iter().copied()).expect("arguments should parse") {
            Command::Run(config) => config,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    fn error(args: &[&str]) -> String {
        match parse_args(args.iter().copied()) {
            Ok(other) => panic!("expected an error, got {other:?}"),
            Err(error) => error,
        }
    }

    #[test]
    fn parses_a_default_tcp_connection() {
        let config = config(&["example.com", "443"]);

        assert!(!config.listen);
        assert_eq!(config.protocol, Protocol::Tcp);
        assert_eq!(config.host.as_deref(), Some("example.com"));
        assert_eq!(config.port, 443);
        assert_eq!(config.family, None);
        assert_eq!(config.timeout, None);
    }

    #[test]
    fn parses_listen_shorthand() {
        let config = config(&["-l", "8080"]);

        assert!(config.listen);
        assert_eq!(config.host, None);
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn parses_listen_with_a_bind_host() {
        let config = config(&["-l", "127.0.0.1", "8080"]);

        assert!(config.listen);
        assert_eq!(config.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn folds_p_into_the_listen_port() {
        let config = config(&["-l", "-p", "9000", "localhost"]);

        assert!(config.listen);
        assert_eq!(config.host.as_deref(), Some("localhost"));
        assert_eq!(config.port, 9000);
        assert_eq!(config.local_port, None);
    }

    #[test]
    fn parses_combined_short_options() {
        let config = config(&["-lvu", "-p5353", "-s", "::1", "-w=1.5", "-n", "-N"]);

        assert!(config.listen);
        assert_eq!(config.protocol, Protocol::Udp);
        assert_eq!(config.family, None);
        assert!(config.verbose);
        assert!(config.numeric);
        assert!(config.shutdown_on_eof);
        assert_eq!(config.port, 5353);
        assert_eq!(config.source, Some("::1".parse().unwrap()));
        assert_eq!(config.timeout, Some(Duration::from_millis(1_500)));
    }

    #[test]
    fn parses_long_options_and_source_port() {
        let config = config(&[
            "--local-port=50000",
            "--source",
            "127.0.0.1",
            "--wait",
            "2",
            "--ipv4",
            "--verbose",
            "example.com",
            "80",
        ]);

        assert_eq!(config.local_port, Some(50_000));
        assert_eq!(config.source, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(config.timeout, Some(Duration::from_secs(2)));
        assert_eq!(config.family, Some(IpVersion::V4));
        assert!(config.verbose);
    }

    #[test]
    fn supports_the_option_terminator() {
        let config = config(&["--", "-example.com", "80"]);

        assert_eq!(config.host.as_deref(), Some("-example.com"));
    }

    #[test]
    fn parses_help_and_version() {
        assert_eq!(parse_args(["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_args(["-V"]).unwrap(), Command::Version);
        assert_eq!(parse_args(["-h"]).unwrap(), Command::Help);
    }

    #[test]
    fn rejects_conflicting_ip_versions() {
        assert!(error(&["-4", "-6", "host", "80"]).contains("cannot be used together"));
    }

    #[test]
    fn rejects_zero_scan_modes_that_do_not_make_sense() {
        assert!(error(&["-z", "-u", "host", "80"]).contains("TCP"));
        assert!(error(&["-z", "-l", "8080"]).contains("connect mode"));
    }

    #[test]
    fn rejects_keep_open_without_listen() {
        assert!(error(&["-k", "host", "80"]).contains("requires -l"));
    }

    #[test]
    fn rejects_missing_and_invalid_ports() {
        assert!(error(&["host"]).contains("host and a port"));
        assert!(error(&["host", "0"]).contains("port 0"));
        assert!(error(&["host", "not-a-port"]).contains("invalid port"));
        assert!(error(&["host", "70000"]).contains("invalid port"));
    }

    #[test]
    fn rejects_bad_timeouts_and_unknown_options() {
        assert!(error(&["-w", "0", "host", "80"]).contains("positive"));
        assert!(error(&["--wat", "host", "80"]).contains("unknown option"));
        assert!(error(&["-Q", "host", "80"]).contains("unknown option"));
    }

    #[test]
    fn rejects_a_listen_port_duplicated_in_two_places() {
        assert!(error(&["-l", "-p", "8080", "127.0.0.1", "8080"]).contains("not both"));
    }
}
