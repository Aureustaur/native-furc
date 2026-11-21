//! Furcadia line-oriented TCP protocol types, parser, and command encoder.

use std::fmt;

/// Default Furcadia game server host and port.
pub const DEFAULT_HOST: &str = "lightbringer.furcadia.com";
pub const DEFAULT_PORT: u16 = 6500;

/// Errors returned when parsing or encoding protocol messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Outgoing command contains a newline or carriage return.
    InvalidNewline,
    /// Outgoing argument is empty where a non-empty value is required.
    EmptyField(&'static str),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNewline => write!(f, "command must not contain CR or LF characters"),
            Self::EmptyField(field) => write!(f, "field '{field}' must not be empty"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod binary_protocol_tests {
    use super::*;

    #[test]
    fn parses_binary_brackets_and_use_map_without_utf8_loss() {
        let mut s_pkt = b"]s  !\"12345678 Dream".to_vec();
        s_pkt.push(0x92);
        s_pkt.extend_from_slice(b"s Title\n");
        assert_eq!(
            ServerMessage::parse_bytes(&s_pkt),
            ServerMessage::ExtendedBytes {
                code: b's',
                data: s_pkt[2..s_pkt.len() - 1].to_vec(),
            }
        );

        let scale_pkt = b"]_####x\n";
        assert_eq!(
            ServerMessage::parse_bytes(scale_pkt),
            ServerMessage::ExtendedBytes {
                code: b'_',
                data: b"####x".to_vec(),
            }
        );

        let offset_pkt = [b']', b'H', b'#', b'#', b'#', b'#', 0xFF, b'#', b'\n'];
        assert_eq!(
            ServerMessage::parse_bytes(&offset_pkt),
            ServerMessage::ExtendedBytes {
                code: b'H',
                data: offset_pkt[2..8].to_vec(),
            }
        );

        let gloam_pkt = b"]O####FF8040##\n";
        assert_eq!(
            ServerMessage::parse_bytes(gloam_pkt),
            ServerMessage::ExtendedBytes {
                code: b'O',
                data: b"####FF8040##".to_vec(),
            }
        );

        assert_eq!(
            ServerMessage::parse_bytes(b";vinca\n"),
            ServerMessage::UseMap("vinca".to_string())
        );
        assert_eq!(
            ServerMessage::parse_bytes(b";\n"),
            ServerMessage::UseMap(String::new())
        );
    }
}

/// Parsed server-to-client protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// Server handshake banner (`Dragonroar ...`), signaling readiness for login.
    Dragonroar { version: Option<String>, raw: String },
    /// Login/session acknowledgment marker (`&&&&&&...`).
    LoginAck,
    /// Chat, MOTD, or system text line (`(` prefix).
    Text(String),
    /// Online status response from `onln` (`]%1<name>` online, `]%0<name>` offline).
    OnlineStatus { online: bool, name: String },
    /// Logged-in character identity (`]B<uid> <name>`).
    CharacterIdentity { uid: String, name: String },
    /// Current dream/map name (`]m<map_name>`).
    MapName(String),
    /// Permanent or alternate map directive (`;<map_name>`, empty resets map).
    UseMap(String),
    /// Binary / Base220 world packet (`<`, `/`, `)`, `>`, `1`, `2`, `4`, `5`, `6`, `7`, `8`, `9`, `@`, `A`..=`F`) with raw uncorrupted bytes.
    WorldPacket { opcode: u8, data: Vec<u8> },
    /// Raw binary region defaults/visibility directive (`]W<data>`) before UTF-8 lossy conversion.
    RegionDefaults(Vec<u8>),
    /// Raw binary extended `]`-prefixed directive (`]s`, `]t`, `]O`, `]_`, `]H`, `]f`, `]}`, `]M`, `]I`, etc.).
    ExtendedBytes { code: u8, data: Vec<u8> },
    /// Extended `]`-prefixed server directive (e.g. `]C`, `]q`, `]f`, `]v`).
    Extended { code: Option<char>, payload: String },
    /// Unrecognized or future-milestone server line preserved verbatim.
    Raw(String),
}

impl ServerMessage {
    /// Parses a single `\n`-terminated raw server line (byte-safe for Base220 / Latin-1).
    pub fn parse_bytes(line: &[u8]) -> Self {
        let mut end = line.len();
        while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
            end -= 1;
        }
        let trimmed = &line[..end];
        if !trimmed.starts_with(b"Dragonroar") {
            if let Some(rest) = trimmed.strip_prefix(b"]W") {
                return Self::RegionDefaults(rest.to_vec());
            }
            if let Some(rest) = trimmed.strip_prefix(b"]") {
                if let Some((&code, payload)) = rest.split_first() {
                    if matches!(
                        code,
                        b's' | b't' | b'O' | b'_' | b'H' | b'f' | b'}' | b'M' | b'I' | b'?' | b'&'
                    ) {
                        return Self::ExtendedBytes {
                            code,
                            data: payload.to_vec(),
                        };
                    }
                    if !matches!(code, b'%' | b'B' | b'm')
                        && std::str::from_utf8(payload).is_err()
                    {
                        return Self::ExtendedBytes {
                            code,
                            data: payload.to_vec(),
                        };
                    }
                }
            }
            if let Some(rest) = trimmed.strip_prefix(b";") {
                return Self::UseMap(crate::encoding::decode_win1252(rest));
            }
            if let Some((&first, rest)) = trimmed.split_first() {
                if matches!(
                    first,
                    b'<' | b'/'
                        | b')'
                        | b'>'
                        | b'1'
                        | b'2'
                        | b'4'
                        | b'5'
                        | b'6'
                        | b'7'
                        | b'8'
                        | b'9'
                        | b'@'
                        | b'A'
                        | b'B'
                        | b'C'
                        | b'D'
                        | b'E'
                        | b'F'
                ) {
                    return Self::WorldPacket {
                        opcode: first,
                        data: rest.to_vec(),
                    };
                }
            }
        }
        let text = String::from_utf8_lossy(trimmed);
        Self::parse(&text)
    }

    /// Parses a single `\n`-terminated (already trimmed of `\r\n`) server line.
    pub fn parse(line: &str) -> Self {
        let trimmed = line.trim_end_matches(['\r', '\n']);

        if trimmed.starts_with("Dragonroar") {
            let version = trimmed
                .split_whitespace()
                .nth(1)
                .map(ToOwned::to_owned);
            return Self::Dragonroar {
                version,
                raw: trimmed.to_owned(),
            };
        }

        if trimmed.len() >= 6 && trimmed.bytes().all(|b| b == b'&') {
            return Self::LoginAck;
        }

        if let Some(rest) = trimmed.strip_prefix('(') {
            return Self::Text(rest.to_owned());
        }

        if let Some(rest) = trimmed.strip_prefix(']') {
            let mut chars = rest.chars();
            let code = chars.next();
            let payload = chars.as_str();
            match code {
                Some('%') => {
                    if let Some(name) = payload.strip_prefix('1') {
                        return Self::OnlineStatus {
                            online: true,
                            name: name.to_owned(),
                        };
                    }
                    if let Some(name) = payload.strip_prefix('0') {
                        return Self::OnlineStatus {
                            online: false,
                            name: name.to_owned(),
                        };
                    }
                }
                Some('B') => {
                    if let Some((uid, name)) = payload.split_once(' ') {
                        return Self::CharacterIdentity {
                            uid: uid.to_owned(),
                            name: name.to_owned(),
                        };
                    }
                }
                Some('m') => {
                    return Self::MapName(payload.to_owned());
                }
                _ => {}
            }
            return Self::Extended {
                code,
                payload: payload.to_owned(),
            };
        }

        Self::Raw(trimmed.to_owned())
    }
}

/// Client-to-server protocol commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCommand {
    /// Legacy/character login: `connect <name> <password>`.
    Connect { name: String, password: String },
    /// Account login: `account <email> <character> <password>`.
    Account {
        email: String,
        character: String,
        password: String,
    },
    /// Update character description: `desc <text>`.
    Desc(String),
    /// Update character color/species code: `color <data>`.
    Color(String),
    /// Local speech: `"<text>`.
    Say(String),
    /// Whisper to a player: `wh <target> <message>`.
    Whisper { target: String, message: String },
    /// Movement step (`1`, `3`, `7`, `9` in Furcadia's isometric grid).
    Move(u8),
    /// Check if a character is online (`onln <name>`).
    OnlineCheck(String),
    /// Inspect a character's profile/description (`glook <name>`).
    Glook(String),
    /// List characters in the current dream (`who`).
    Who,
    /// Disconnect cleanly: `quit`.
    Quit,
    /// Raw protocol line (without trailing `\n`).
    Raw(String),
}

fn ensure_single_line(s: &str) -> Result<(), ProtocolError> {
    if s.contains(['\r', '\n']) {
        Err(ProtocolError::InvalidNewline)
    } else {
        Ok(())
    }
}

/// Furcadia parses raw `<` in say/whisper payloads as HTML markup, so any
/// literal `<` (such as `>.<`, `<3`, `a < b`, or `<blushes>`) breaks the
/// server's surrounding `<name>`/`</font>` tags unless entity-escaped on the
/// wire. Preserve player formatting tags (`<b>`, `<i>`, `<u>`, `<strong>`,
/// `<em>`, `<a href=...>`, and their closing tags).
fn escape_chat_markup(text: &str) -> String {
    if !text.contains('<') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after_lt = &rest[lt + 1..];
        if let Some(gt) = after_lt.find('>') {
            let inside = &after_lt[..gt];
            if is_allowed_chat_tag(inside) {
                out.push('<');
                out.push_str(inside);
                out.push('>');
                rest = &after_lt[gt + 1..];
                continue;
            }
        }
        out.push_str("&lt;");
        rest = after_lt;
    }
    out.push_str(rest);
    out
}

fn is_allowed_chat_tag(inside: &str) -> bool {
    if inside.is_empty() || inside.contains('<') {
        return false;
    }
    let trimmed = inside.trim();
    if let Some(closing) = trimmed.strip_prefix('/') {
        let name = closing.trim().to_ascii_lowercase();
        return matches!(name.as_str(), "b" | "i" | "u" | "strong" | "em" | "a");
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), "b" | "i" | "u" | "strong" | "em") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix('a') {
        return rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_whitespace())
            && rest.contains("href=");
    }
    false
}

const PW_BASE_INFO: &str = "IP=192.168.0.3 H=155d414c R=16587 D=2087772 U=8242621 T=bcgfefdfa M=1600x900 V=31.2-Final ";
const PW_CLIENT_SALT: &str = "168356072050782049568476";

fn md5_hex(data: &[u8]) -> String {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    const SHIFTS: [[u32; 4]; 4] = [
        [7, 12, 17, 22],
        [5, 9, 14, 20],
        [4, 11, 16, 23],
        [6, 10, 15, 21],
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (idx, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            m[idx] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for (i, &k) in K.iter().enumerate() {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let s = SHIFTS[i / 16][i % 4];
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                f.wrapping_add(a)
                    .wrapping_add(k)
                    .wrapping_add(m[g])
                    .rotate_left(s),
            );
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            use std::fmt::Write;
            let _ = write!(&mut out, "{byte:02x}");
        }
    }
    out
}

fn pw_swap_even_mirror(slice: &mut [u8]) {
    let n = slice.len();
    let half = n / 2;
    let mut i = 0;
    while i < half {
        slice.swap(i, n - 1 - i);
        i += 2;
    }
}

fn pw_swap_pair_mirror(slice: &mut [u8]) {
    let n = slice.len();
    let q = n / 4;
    for i in 0..q {
        let a0 = 2 * i;
        let a1 = 2 * i + 1;
        let b0 = n - 1 - 2 * i;
        let b1 = n - 2 - 2 * i;
        let t0 = slice[a0];
        let t1 = slice[a1];
        slice[a0] = slice[b0];
        slice[a1] = slice[b1];
        slice[b0] = t0;
        slice[b1] = t1;
    }
}

/// Applies the Furcadia `PW` login payload permutation (`0x40efc0`).
pub fn scramble_pw_payload(buf: &mut [u8]) {
    if buf.len() <= 8 {
        return;
    }
    pw_swap_even_mirror(&mut buf[0..]);
    pw_swap_pair_mirror(&mut buf[1..]);
    pw_swap_pair_mirror(&mut buf[2..]);
    pw_swap_even_mirror(&mut buf[3..]);
    pw_swap_even_mirror(&mut buf[4..]);
    pw_swap_pair_mirror(&mut buf[5..]);
    pw_swap_pair_mirror(&mut buf[6..]);
    pw_swap_even_mirror(&mut buf[7..]);
    pw_swap_pair_mirror(&mut buf[8..]);
}

/// Reverses the Furcadia `PW` login payload permutation (`0x40f020`).
pub fn unscramble_pw_payload(buf: &mut [u8]) {
    if buf.len() <= 8 {
        return;
    }
    pw_swap_pair_mirror(&mut buf[8..]);
    pw_swap_even_mirror(&mut buf[7..]);
    pw_swap_pair_mirror(&mut buf[6..]);
    pw_swap_pair_mirror(&mut buf[5..]);
    pw_swap_even_mirror(&mut buf[4..]);
    pw_swap_even_mirror(&mut buf[3..]);
    pw_swap_pair_mirror(&mut buf[2..]);
    pw_swap_pair_mirror(&mut buf[1..]);
    pw_swap_even_mirror(&mut buf[0..]);
}

/// Builds the signed and scrambled `PW` token body (without the `PW` prefix).
pub fn build_pw_token() -> String {
    let sig_input = format!("{PW_BASE_INFO}P={PW_CLIENT_SALT}");
    let sig = md5_hex(sig_input.as_bytes());
    let mut buf = format!("{PW_BASE_INFO}S={sig}").into_bytes();
    scramble_pw_payload(&mut buf);
    String::from_utf8(buf).expect("ASCII PW payload")
}

impl ClientCommand {
    /// Serializes the command into a wire-ready line ending with `\n`.
    pub fn encode(&self) -> Result<String, ProtocolError> {
        let line = match self {
            Self::Connect { name, password } => {
                if name.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("name"));
                }
                if password.is_empty() {
                    return Err(ProtocolError::EmptyField("password"));
                }
                ensure_single_line(name)?;
                ensure_single_line(password)?;
                let wire_name = name.trim().replace([' ', '|'], "_");
                let wire_pass = password.replace(' ', "_");
                format!("connect {wire_name} {wire_pass}")
            }
            Self::Account {
                email,
                character,
                password,
            } => {
                if email.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("email"));
                }
                if character.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("character"));
                }
                if password.is_empty() {
                    return Err(ProtocolError::EmptyField("password"));
                }
                ensure_single_line(email)?;
                ensure_single_line(character)?;
                ensure_single_line(password)?;
                let wire_email = email.trim().replace(' ', "_");
                let wire_char = character.trim().replace([' ', '|'], "_");
                let wire_pass = password.replace(' ', "_");
                format!("account {wire_email} {wire_char} {wire_pass}")
            }
            Self::Desc(text) => {
                ensure_single_line(text)?;
                format!("desc {text}")
            }
            Self::Color(data) => {
                if data.is_empty() {
                    return Err(ProtocolError::EmptyField("color"));
                }
                ensure_single_line(data)?;
                format!("color {data}")
            }
            Self::Say(text) => {
                ensure_single_line(text)?;
                format!("\"{}", escape_chat_markup(text))
            }
            Self::Whisper { target, message } => {
                if target.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("target"));
                }
                ensure_single_line(target)?;
                ensure_single_line(message)?;
                format!("wh {target} {}", escape_chat_markup(message))
            }
            Self::Move(dir) => format!("m {dir}"),
            Self::OnlineCheck(name) => {
                if name.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("name"));
                }
                ensure_single_line(name)?;
                format!("onln {name}")
            }
            Self::Glook(name) => {
                if name.trim().is_empty() {
                    return Err(ProtocolError::EmptyField("name"));
                }
                ensure_single_line(name)?;
                format!("glook {name}")
            }
            Self::Who => "who".to_owned(),
            Self::Quit => "quit".to_owned(),
            Self::Raw(raw) => {
                ensure_single_line(raw)?;
                raw.clone()
            }
        };
        Ok(format!("{line}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dragonroar_and_ack() {
        assert_eq!(
            ServerMessage::parse("Dragonroar V00013\r\n"),
            ServerMessage::Dragonroar {
                version: Some("V00013".into()),
                raw: "Dragonroar V00013".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse_bytes(b"Dragonroar\n"),
            ServerMessage::Dragonroar {
                version: None,
                raw: "Dragonroar".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse_bytes(b"Dragonroar V00013\r\n"),
            ServerMessage::Dragonroar {
                version: Some("V00013".into()),
                raw: "Dragonroar V00013".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse_bytes(b"D!!##\n"),
            ServerMessage::WorldPacket {
                opcode: b'D',
                data: b"!!##".to_vec(),
            }
        );
        assert_eq!(ServerMessage::parse("&&&&&&\n"), ServerMessage::LoginAck);
        assert_eq!(
            ServerMessage::parse("(Welcome to Furcadia!"),
            ServerMessage::Text("Welcome to Furcadia!".into())
        );
        assert_eq!(
            ServerMessage::parse("]B43091366 Tactful"),
            ServerMessage::CharacterIdentity {
                uid: "43091366".into(),
                name: "Tactful".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse("]%1Tactful"),
            ServerMessage::OnlineStatus {
                online: true,
                name: "Tactful".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse("]%0Nobody"),
            ServerMessage::OnlineStatus {
                online: false,
                name: "Nobody".into(),
            }
        );
        assert_eq!(
            ServerMessage::parse("]marco 99"),
            ServerMessage::MapName("arco 99".into())
        );
        assert_eq!(
            ServerMessage::parse_bytes(b">C#W#K+\n"),
            ServerMessage::WorldPacket {
                opcode: b'>',
                data: b"C#W#K+".to_vec(),
            }
        );
        assert_eq!(
            ServerMessage::parse_bytes(b"@  ##\n"),
            ServerMessage::WorldPacket {
                opcode: b'@',
                data: b"  ##".to_vec(),
            }
        );
        assert_eq!(
            ServerMessage::parse_bytes(&[b']', b'W', 0xfe, 0x23]),
            ServerMessage::RegionDefaults(vec![0xfe, 0x23])
        );
    }

    #[test]
    fn encode_commands_and_reject_newlines() {
        let cmd = ClientCommand::Connect {
            name: "Hero Name".into(),
            password: "secret".into(),
        };
        assert_eq!(cmd.encode().unwrap(), "connect Hero_Name secret\n");
        assert_eq!(
            ClientCommand::OnlineCheck("Tactful".into()).encode().unwrap(),
            "onln Tactful\n"
        );
        assert_eq!(
            ClientCommand::Glook("Tactful".into()).encode().unwrap(),
            "glook Tactful\n"
        );

        let acct = ClientCommand::Account {
            email: "player@example.com".into(),
            character: "Hero Name".into(),
            password: "secret".into(),
        };
        assert_eq!(
            acct.encode().unwrap(),
            "account player@example.com Hero_Name secret\n"
        );
        let piped = ClientCommand::Account {
            email: "player@example.com".into(),
            character: "Hero|Name".into(),
            password: "secret".into(),
        };
        assert_eq!(
            piped.encode().unwrap(),
            "account player@example.com Hero_Name secret\n"
        );

        let bad = ClientCommand::Say("hello\nquit".into());
        assert_eq!(bad.encode(), Err(ProtocolError::InvalidNewline));
    }

    #[test]
    fn pw_token_roundtrip_and_md5_rfc1321() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );

        let scrambled = build_pw_token();
        let mut bytes = scrambled.into_bytes();
        unscramble_pw_payload(&mut bytes);
        let plain = String::from_utf8(bytes).unwrap();
        assert!(plain.starts_with("IP=192.168.0.3 "));
        let (prefix, sig) = plain
            .split_once("S=")
            .expect("unscrambled PW payload must contain S=");
        assert_eq!(prefix, PW_BASE_INFO);
        assert_eq!(sig.len(), 32);
        let expected_sig = md5_hex(format!("{prefix}P={PW_CLIENT_SALT}").as_bytes());
        assert_eq!(sig, expected_sig);
    }

    #[test]
    fn encode_chat_and_whisper_escapes_literal_lt_but_keeps_formatting_tags() {
        assert_eq!(
            ClientCommand::Whisper {
                target: "Harry|Hardacre".into(),
                message: "oops >.<".into(),
            }
            .encode()
            .unwrap(),
            "wh Harry|Hardacre oops >.&lt;\n"
        );
        assert_eq!(
            ClientCommand::Whisper {
                target: "Harry|Hardacre".into(),
                message: "<b>bold</b> and <i>italic >.<</i> <3 a < b".into(),
            }
            .encode()
            .unwrap(),
            "wh Harry|Hardacre <b>bold</b> and <i>italic >.&lt;</i> &lt;3 a &lt; b\n"
        );
        assert_eq!(
            ClientCommand::Say("<a href='https://example.com'>hi</a> <blushes>".into())
                .encode()
                .unwrap(),
            "\"<a href='https://example.com'>hi</a> &lt;blushes>\n"
        );
    }
}
