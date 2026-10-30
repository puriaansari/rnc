# rnc

A compact, netcat-style TCP/UDP client and listener written in Rust.

`rnc` supports the everyday netcat workflows: piping data to a remote port,
serving a one-shot connection, keeping a listener open, sending UDP datagrams,
scanning TCP ports, and choosing the source address/port.

## Build

```text
cargo build --release
```

The binary is written to `target/release/rnc.exe` on Windows (or
`target/release/rnc` elsewhere). A prebuilt Windows x64 release is also included
at `dist/rnc.exe`. Copy or rename it to `nc` if you want to use it as a drop-in
command.

On Windows, the default Rust MSVC target needs the Visual Studio Build Tools
with the **Desktop development with C++** workload. A MinGW toolchain and the
`x86_64-pc-windows-gnu` Rust target also work.

## Usage

```text
rnc [OPTIONS] <host> <port>          connect
rnc [OPTIONS] -l [host] <port>       listen
rnc [OPTIONS] -l -p <port> [host]    listen with -p
```

Run `rnc --help` for the full option list.

| Option | Meaning |
| --- | --- |
| `-4`, `--ipv4` | Resolve and connect over IPv4 only |
| `-6`, `--ipv6` | Resolve and connect over IPv6 only |
| `-u`, `--udp` | Use UDP instead of TCP |
| `-l`, `--listen` | Listen instead of connecting |
| `-k`, `--keep-open` | Accept connections until interrupted |
| `-p`, `--local-port <PORT>` | Listen port, or source port when connecting |
| `-s`, `--source <ADDR>` | Local source IP address |
| `-w`, `--wait <SECONDS>` | Connect/read timeout; fractions are allowed |
| `-N`, `--shutdown` | Half-close the TCP write side when stdin reaches EOF |
| `-z`, `--zero` | TCP connect scan only; do not send data |
| `-n`, `--numeric` | Require a numeric IP instead of resolving a name |
| `-v`, `--verbose` | Print connection details to stderr |

## Examples

The examples below use POSIX shell syntax. In PowerShell, replace `printf ... |`
with a pipeline such as `"ping" | .\dist\rnc.exe ...`, and use `.\dist\rnc.exe`
as the command path.

Connect to an HTTP server and print the response:

```text
printf 'GET / HTTP/1.0\r\nHost: example.com\r\n\r\n' | rnc example.com 80
```

Serve one connection on port 8080. Data typed into the terminal is sent to the
client, and data from the client is printed:

```text
rnc -l 8080
```

Keep serving connections after each one closes:

```text
rnc -l -k 8080
```

Send a file and half-close the write side so the peer knows the request ended:

```text
rnc -N example.com 9000 < payload.bin > response.bin
```

Receive UDP datagrams. The sender of the most recent datagram becomes the
destination for anything typed into stdin:

```text
rnc -u -l 5353
```

Send one UDP datagram and wait up to two seconds for replies:

```text
printf 'ping' | rnc -u -w 2 192.0.2.10 5353
```

Scan TCP ports. `-z` exits with status 0 when the connection succeeds:

```text
rnc -zv -w 2 192.168.1.10 22
```

Bind a specific source address and port:

```text
rnc -s 192.168.1.50 -p 40000 example.com 80
```

## Behavior notes

- **TCP half-close:** When stdin ends, `rnc` keeps the TCP connection open so a
  response can still arrive. Use `-N` to send FIN after stdin EOF. This is
  important for request/response servers that wait for EOF.
- **Timeouts:** `-w` applies to the TCP connect, subsequent socket reads and
  writes, and the UDP linger period. Without `-w`, TCP reads block until the
  peer closes.
- **UDP linger:** UDP has no connection close. After stdin ends, the UDP client
  waits for a final response for one second by default, or for `-w` seconds.
  Receiving a datagram resets that timer.
- **Listener throughput:** A TCP listener handles one connection at a time.
  `-k` accepts the next connection after the previous one finishes. Stdin is
  shared across those connections through a single relay.
- **UDP listen:** Without a known sender, stdin data waits until the first
  datagram arrives. Replies go to the most recent sender.
- **No command execution:** The classic `-e` option is intentionally not
  implemented. It turns a network service into arbitrary remote command
  execution.

Service names such as `http` are not resolved; use the numeric port. Everything
is plain TCP/UDP—there is no TLS or proxy support.

## Tests

```text
cargo test
```

The unit tests cover option parsing and validation. The end-to-end tests start
real `rnc` client/listener processes and check TCP relay, TCP listener,
keep-open stdin reuse, UDP client datagrams, and UDP listener replies.

## Layout

- `src/cli.rs` — argument parsing, help text, validation, unit tests
- `src/net.rs` — TCP/UDP connect and listen paths, relay loops, timeouts
- `src/main.rs` — process entry point and error reporting
- `tests/e2e.rs` — end-to-end network tests

Only `socket2` is used, for source address/port binding and connect timeouts.
