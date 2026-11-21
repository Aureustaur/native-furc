//! TCP client and login state machine for the Furcadia protocol.
//!
//! # Connection Lifecycle
//! 1. [`FurcadiaClient::connect`] opens a plain TCP socket and enters
//!    [`SessionState::AwaitingDragonroar`].
//! 2. [`FurcadiaClient::wait_for_handshake`] reads lines until the server sends
//!    its `Dragonroar` banner (`SessionState::Handshaked`).
//! 3. [`FurcadiaClient::enable_tls`] negotiates STARTTLS (currently using legacy
//!    TLS 1.2 compatibility settings because Furcadia's server presents an expired
//!    certificate and 1024-bit DHE).
//! 4. [`FurcadiaClient::login_account`] / [`FurcadiaClient::login_character`]
//!    sends credentials (`SessionState::Authenticating`) until the server replies
//!    with `&&&&&&` (`SessionState::Connected`).

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::protocol::{ClientCommand, ProtocolError, ServerMessage, DEFAULT_HOST, DEFAULT_PORT};

/// Maximum length of a single newline-delimited protocol line (1 MiB).
/// Prevents unbounded memory growth if a misbehaving server streams without `\n`.
const MAX_PROTOCOL_LINE_BYTES: usize = 1024 * 1024;

/// Configuration for connecting to a Furcadia game server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    pub read_timeout: Option<Duration>,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Some(Duration::from_millis(200)),
        }
    }
}

/// Lifecycle state of a Furcadia client session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Disconnected,
    AwaitingDragonroar,
    Handshaked { server_version: Option<String> },
    Authenticating,
    Connected,
}

/// Errors returned by `FurcadiaClient`.
#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Protocol(ProtocolError),
    InvalidState {
        expected: &'static str,
        actual: SessionState,
    },
    Timeout(String),
    Disconnected,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Protocol(err) => write!(f, "protocol error: {err}"),
            Self::InvalidState { expected, actual } => {
                write!(
                    f,
                    "invalid session state: expected {expected}, got {actual:?}"
                )
            }
            Self::Timeout(msg) => write!(f, "connection timed out: {msg}"),
            Self::Disconnected => write!(f, "server closed the connection"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<ProtocolError> for ClientError {
    fn from(err: ProtocolError) -> Self {
        Self::Protocol(err)
    }
}

/// Synchronous, line-framed TCP client for Furcadia.
pub struct FurcadiaClient {
    reader: BufReader<ConnectionStream>,
    host: String,
    read_timeout: Option<Duration>,
    state: SessionState,
    byte_buf: Vec<u8>,
}

enum ConnectionStream {
    Empty,
    Plain(TcpStream),
    Tls(openssl::ssl::SslStream<TcpStream>),
}

impl Read for ConnectionStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Empty => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection is closed",
            )),
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for ConnectionStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Empty => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection is closed",
            )),
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Empty => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection is closed",
            )),
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl FurcadiaClient {
    /// Opens a TCP connection using `config` and enters `SessionState::AwaitingDragonroar`.
    /// Tries each resolved address in order so dual-stack hosts fall back cleanly if IPv6 fails.
    pub fn connect(config: &ConnectionConfig) -> Result<Self, ClientError> {
        let addrs: Vec<_> = format!("{}:{}", config.host, config.port)
            .to_socket_addrs()?
            .collect();
        if addrs.is_empty() {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "could not resolve host",
            )));
        }

        let mut last_err = None;
        let mut connected = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, config.connect_timeout) {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(err) => last_err = Some(err),
            }
        }
        let stream = match connected {
            Some(s) => s,
            None => {
                return Err(ClientError::Io(last_err.unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrNotAvailable, "could not connect to host")
                })))
            }
        };
        stream.set_read_timeout(config.read_timeout)?;
        Ok(Self {
            reader: BufReader::new(ConnectionStream::Plain(stream)),
            host: config.host.clone(),
            read_timeout: config.read_timeout,
            state: SessionState::AwaitingDragonroar,
            byte_buf: Vec::with_capacity(512),
        })
    }

    /// Reads bytes into `self.byte_buf` until `\n` or EOF, enforcing [`MAX_PROTOCOL_LINE_BYTES`].
    fn read_line_bounded(&mut self) -> io::Result<usize> {
        let remaining = MAX_PROTOCOL_LINE_BYTES.saturating_sub(self.byte_buf.len());
        if remaining == 0 {
            self.byte_buf.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server line exceeded maximum allowed length",
            ));
        }
        let mut limited = (&mut self.reader).take((remaining + 1) as u64);
        let n = limited.read_until(b'\n', &mut self.byte_buf)?;
        if self.byte_buf.len() > MAX_PROTOCOL_LINE_BYTES && !self.byte_buf.ends_with(b"\n") {
            self.byte_buf.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server line exceeded maximum allowed length",
            ));
        }
        Ok(n)
    }

    /// Requests Furcadia's STARTTLS upgrade. Uses legacy compatibility settings
    /// because the game server currently presents an expired certificate and
    /// requires a 1024-bit DHE exchange. The caller must warn the user clearly.
    pub fn enable_tls(&mut self, timeout: Duration) -> Result<Vec<ServerMessage>, ClientError> {
        if !matches!(self.state, SessionState::Handshaked { .. }) {
            return Err(ClientError::InvalidState {
                expected: "Handshaked",
                actual: self.state.clone(),
            });
        }
        self.reader.get_mut().write_all(b"starttls\n")?;
        self.reader.get_mut().flush()?;

        let deadline = Instant::now() + timeout;
        let mut pending = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout("timed out negotiating TLS".into()));
            }
            match self.read_line_bounded() {
                Ok(0) => return Err(ClientError::Disconnected),
                Ok(_) => {
                    let msg = ServerMessage::parse_bytes(&self.byte_buf);
                    self.byte_buf.clear();
                    match &msg {
                        ServerMessage::Extended {
                            code: Some('S'),
                            payload,
                        } if payload.starts_with('1') => break,
                        ServerMessage::Extended {
                            code: Some('S'),
                            payload,
                        } => {
                            return Err(ClientError::Io(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                format!("server refused TLS (status {payload})"),
                            )));
                        }
                        _ => pending.push(msg),
                    }
                }
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(err) => return Err(ClientError::Io(err)),
            }
        }

        if !self.reader.buffer().is_empty() {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected plaintext buffered after TLS readiness response",
            )));
        }
        let reader = std::mem::replace(&mut self.reader, BufReader::new(ConnectionStream::Empty));
        let stream = match reader.into_inner() {
            ConnectionStream::Plain(stream) => stream,
            ConnectionStream::Tls(_) | ConnectionStream::Empty => {
                self.state = SessionState::Disconnected;
                return Err(ClientError::InvalidState {
                    expected: "plain connection",
                    actual: self.state.clone(),
                })
            }
        };

        let tls_result = (|| -> Result<openssl::ssl::SslStream<TcpStream>, ClientError> {
            stream.set_read_timeout(Some(timeout))?;
            let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())
                .map_err(tls_error)?;
            builder
                .set_min_proto_version(Some(openssl::ssl::SslVersion::TLS1_2))
                .map_err(tls_error)?;
            builder
                .set_max_proto_version(Some(openssl::ssl::SslVersion::TLS1_2))
                .map_err(tls_error)?;
            builder
                .set_cipher_list("DEFAULT:@SECLEVEL=0")
                .map_err(tls_error)?;
            builder.set_verify(openssl::ssl::SslVerifyMode::NONE);
            let connector = builder.build();
            let tls_stream = connector.connect(&self.host, stream).map_err(tls_error)?;
            tls_stream.get_ref().set_read_timeout(self.read_timeout)?;
            Ok(tls_stream)
        })();
        match tls_result {
            Ok(tls_stream) => {
                self.reader = BufReader::new(ConnectionStream::Tls(tls_stream));
                Ok(pending)
            }
            Err(err) => {
                self.state = SessionState::Disconnected;
                Err(err)
            }
        }
    }

    /// Returns the current session state.
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Reads and parses the next server message if available before `read_timeout`.
    /// Returns `Ok(None)` on read timeout / `WouldBlock`.
    pub fn poll_message(&mut self) -> Result<Option<ServerMessage>, ClientError> {
        match self.read_line_bounded() {
            Ok(0) => {
                self.byte_buf.clear();
                self.state = SessionState::Disconnected;
                Err(ClientError::Disconnected)
            }
            Ok(_) => {
                let msg = ServerMessage::parse_bytes(&self.byte_buf);
                self.byte_buf.clear();
                self.advance_state(&msg);
                Ok(Some(msg))
            }
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(err) => {
                self.byte_buf.clear();
                Err(ClientError::Io(err))
            }
        }
    }

    /// Polls until the `Dragonroar` handshake banner is received (with a default
    /// 15-second deadline), returning any pre-handshake lines encountered along the way.
    pub fn wait_for_handshake(&mut self) -> Result<Vec<ServerMessage>, ClientError> {
        self.wait_for_handshake_timeout(Duration::from_secs(15))
    }

    /// Polls until the `Dragonroar` handshake banner is received or `timeout` elapses.
    pub fn wait_for_handshake_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<ServerMessage>, ClientError> {
        let deadline = Instant::now() + timeout;
        let mut pre_handshake = Vec::new();
        loop {
            if matches!(self.state, SessionState::Handshaked { .. }) {
                return Ok(pre_handshake);
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(
                    "timed out waiting for Dragonroar server handshake".to_owned(),
                ));
            }
            if let Some(msg) = self.poll_message()? {
                pre_handshake.push(msg);
            }
        }
    }

    /// Sends a `connect <name> <password>` command after the `Dragonroar` handshake.
    pub fn login_character(
        &mut self,
        name: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<(), ClientError> {
        if !matches!(self.state, SessionState::Handshaked { .. }) {
            return Err(ClientError::InvalidState {
                expected: "Handshaked",
                actual: self.state.clone(),
            });
        }
        self.send_command(&ClientCommand::Connect {
            name: name.into(),
            password: password.into(),
        })?;
        self.state = SessionState::Authenticating;
        Ok(())
    }

    /// Sends an `account <email> <character> <password>` command after the `Dragonroar` handshake.
    pub fn login_account(
        &mut self,
        email: impl Into<String>,
        character: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<(), ClientError> {
        if !matches!(self.state, SessionState::Handshaked { .. }) {
            return Err(ClientError::InvalidState {
                expected: "Handshaked",
                actual: self.state.clone(),
            });
        }
        self.send_command(&ClientCommand::Account {
            email: email.into(),
            character: character.into(),
            password: password.into(),
        })?;
        self.state = SessionState::Authenticating;
        Ok(())
    }

    /// Encodes and writes a `ClientCommand` to the server socket.
    pub fn send_command(&mut self, cmd: &ClientCommand) -> Result<(), ClientError> {
        let encoded = cmd.encode()?;
        self.reader.get_mut().write_all(encoded.as_bytes())?;
        self.reader.get_mut().flush()?;
        Ok(())
    }

    /// Sends an `account <email> <character> <password>` command with a pre-formatted
    /// wire character token (used for `|` fallback after `_` normalization).
    pub fn login_account_raw_char(
        &mut self,
        email: &str,
        wire_character: &str,
        password: &str,
    ) -> Result<(), ClientError> {
        if !matches!(self.state, SessionState::Handshaked { .. }) {
            return Err(ClientError::InvalidState {
                expected: "Handshaked",
                actual: self.state.clone(),
            });
        }
        if email.trim().is_empty()
            || wire_character.trim().is_empty()
            || password.is_empty()
            || email.contains(['\r', '\n'])
            || wire_character.contains(['\r', '\n'])
            || password.contains(['\r', '\n'])
        {
            return Err(ClientError::Protocol(
                crate::protocol::ProtocolError::InvalidNewline,
            ));
        }
        let wire_email = email.trim().replace(' ', "_");
        let wire_char = wire_character.trim();
        let wire_pass = password.replace(' ', "_");
        self.send_command(&ClientCommand::Raw(format!(
            "account {wire_email} {wire_char} {wire_pass}"
        )))?;
        self.state = SessionState::Authenticating;
        Ok(())
    }

    /// Sends `quit` and updates state to `Disconnected`.
    pub fn disconnect(&mut self) -> Result<(), ClientError> {
        let res = self.send_command(&ClientCommand::Quit);
        self.state = SessionState::Disconnected;
        res
    }

    fn advance_state(&mut self, msg: &ServerMessage) {
        match (&self.state, msg) {
            (SessionState::AwaitingDragonroar, ServerMessage::Dragonroar { version, .. }) => {
                self.state = SessionState::Handshaked {
                    server_version: version.clone(),
                };
            }
            (SessionState::Authenticating, ServerMessage::LoginAck) => {
                self.state = SessionState::Connected;
            }
            _ => {}
        }
    }
}

fn tls_error(error: impl std::fmt::Display) -> ClientError {
    ClientError::Io(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("legacy TLS handshake failed: {error}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn mock_server_handshake_and_login() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            conn.write_all(b"(Connecting...\nDragonroar V00013\n")
                .unwrap();
            conn.flush().unwrap();

            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "connect TestFox TestPass\n");

            conn.write_all(b"&&&&&&\n(Welcome to Furcadia!\n").unwrap();
            conn.flush().unwrap();
        });

        let config = ConnectionConfig {
            host: "127.0.0.1".into(),
            port,
            connect_timeout: Duration::from_secs(2),
            read_timeout: Some(Duration::from_millis(500)),
        };

        let mut client = FurcadiaClient::connect(&config).unwrap();
        assert_eq!(*client.state(), SessionState::AwaitingDragonroar);

        let pre = client.wait_for_handshake().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(
            *client.state(),
            SessionState::Handshaked {
                server_version: Some("V00013".into())
            }
        );

        client.login_character("TestFox", "TestPass").unwrap();
        assert_eq!(*client.state(), SessionState::Authenticating);

        let ack = client.poll_message().unwrap().unwrap();
        assert_eq!(ack, ServerMessage::LoginAck);
        assert_eq!(*client.state(), SessionState::Connected);

        let welcome = client.poll_message().unwrap().unwrap();
        assert_eq!(welcome, ServerMessage::Text("Welcome to Furcadia!".into()));

        server.join().unwrap();
    }

    #[test]
    fn timeout_error_formats_clearly() {
        let err = ClientError::Timeout("handshake".into());
        assert!(err.to_string().contains("timed out"));
    }
}
