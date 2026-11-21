//! Furcadia account authentication and character discovery via `charon.furcadia.com`.

use std::fmt;
use std::io::Write;
use std::process::Stdio;

const CLOGIN_ENDPOINTS: &[&str] = &[
    "https://charon.furcadia.com/accounts/clogin.php",
    "https://charon2.furcadia.com/accounts/clogin.php",
    "https://charon3.furcadia.com/accounts/clogin.php",
];

const CLIENT_KEY: &str = "ZAWJuuaTpRrG-Furcadia-KFzsVWwpPM9t";
const CLIENT_VERSION: &str = "31_2_final";
const HTTP_STATUS_MARKER: &str = "\n__HTTP_STATUS__:";

/// A character belonging to a Furcadia account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterSummary {
    pub id: String,
    pub name: String,
    pub species: String,
    pub colors: String,
    /// Last portrait value returned by `clogin.php`; may be a portrait slot index (`1..=16`) or a `pp<id>` file ID (`> 16`).
    pub last_port: Option<u32>,
    pub last_login: String,
    pub desc: String,
}

/// Account metadata and character list returned by `clogin.php`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub account_id: String,
    pub email: String,
    pub characters: Vec<CharacterSummary>,
}

/// Errors returned during account authentication / character discovery.
#[derive(Debug)]
pub enum AuthError {
    Transport(String),
    InvalidResponse(String),
    AuthFailed(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "auth transport error: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid auth response: {msg}"),
            Self::AuthFailed(msg) => write!(f, "authentication failed: {msg}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Queries `charon.furcadia.com/accounts/clogin.php` (with `charon2`/`charon3` fallback)
/// and returns the parsed `AccountInfo` with its character list.
pub fn fetch_account_characters(email: &str, password: &str) -> Result<AccountInfo, AuthError> {
    let mut last_err = None;
    for endpoint in CLOGIN_ENDPOINTS {
        // Keep credentials out of curl's argv: process listings can expose arguments
        // to other local users while the child process is running.
        let body = format!(
            "k={}&v={}&u={}&p={}",
            form_url_encode(CLIENT_KEY),
            form_url_encode(CLIENT_VERSION),
            form_url_encode(email),
            form_url_encode(password),
        );
        let output = crate::sys::command("curl")
            .arg("-sS")
            .arg("-L")
            .arg("--proto-redir")
            .arg("=https")
            .arg("--max-time")
            .arg("10")
            .arg("-w")
            .arg("\n__HTTP_STATUS__:%{http_code}")
            .arg("-X")
            .arg("POST")
            .arg(endpoint)
            .arg("--data-binary")
            .arg("@-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(body.as_bytes())?;
                }
                child.wait_with_output()
            });

        match output {
            Ok(out) if out.status.success() => {
                let raw = String::from_utf8_lossy(&out.stdout);
                let (body, status_code) = split_curl_http_status(&raw);
                if status_code == 400 {
                    return Err(AuthError::AuthFailed(http_400_message(body)));
                }
                if status_code == 0 || status_code >= 500 {
                    last_err = Some(AuthError::Transport(format!(
                        "{endpoint}: HTTP {status_code}"
                    )));
                    continue;
                }
                match parse_clogin_xml(body) {
                    Ok(info) => return Ok(info),
                    Err(err @ AuthError::AuthFailed(_)) => return Err(err),
                    Err(err) => {
                        last_err = Some(err);
                        continue;
                    }
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                last_err = Some(AuthError::Transport(format!("{endpoint}: {stderr}")));
            }
            Err(err) => {
                last_err = Some(AuthError::Transport(format!("failed to run curl: {err}")));
            }
        }
    }

    Err(last_err.unwrap_or_else(|| AuthError::Transport("no auth endpoints available".into())))
}

fn form_url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(char::from_digit((byte >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            encoded.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    encoded
}

/// Parses the XML response from `clogin.php`.
pub fn parse_clogin_xml(xml: &str) -> Result<AccountInfo, AuthError> {
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return Err(AuthError::InvalidResponse("empty auth response".into()));
    }

    if let Some(err_msg) = extract_tag_text(trimmed, "error").filter(|s| !s.is_empty()) {
        return Err(AuthError::AuthFailed(err_msg));
    }

    if looks_like_html(trimmed) {
        return Err(AuthError::InvalidResponse(format!(
            "unexpected HTML response: {}",
            clean_error_body(trimmed)
        )));
    }

    let (account_id, email) = if let Some(account_tag) = find_open_tag(trimmed, "account") {
        (
            extract_attr(account_tag, "id").unwrap_or_default(),
            extract_attr(account_tag, "email").unwrap_or_default(),
        )
    } else {
        if let Some(msg) = extract_notice_tag(trimmed) {
            return Err(AuthError::AuthFailed(msg));
        }
        if find_open_tag(trimmed, "characterselect").is_some()
            && find_next_open_tag_offset(trimmed, "char").is_some()
        {
            (String::new(), String::new())
        } else {
            let plain = clean_error_body(trimmed);
            if !plain.is_empty() {
                return Err(AuthError::AuthFailed(plain));
            }
            return Err(AuthError::InvalidResponse(
                "missing <account> element".into(),
            ));
        }
    };

    let mut characters = Vec::new();
    let mut rest = trimmed;
    while let Some((start, end)) = find_next_open_tag_offset(rest, "char") {
        let char_tag = &rest[start..=end];
        let after_tag = &rest[end + 1..];

        let desc = if char_tag.ends_with("/>") {
            rest = after_tag;
            String::new()
        } else if let Some(close_idx) = after_tag.find("</char>") {
            let inner = &after_tag[..close_idx];
            rest = &after_tag[close_idx + "</char>".len()..];
            extract_tag_text(inner, "desc").unwrap_or_default()
        } else {
            rest = after_tag;
            String::new()
        };

        let raw_name = extract_attr(char_tag, "name").unwrap_or_default();
        characters.push(CharacterSummary {
            id: extract_attr(char_tag, "id").unwrap_or_default(),
            name: raw_name.replace('|', " "),
            species: extract_attr(char_tag, "species").unwrap_or_default(),
            colors: extract_attr(char_tag, "colors").unwrap_or_default(),
            last_port: extract_attr(char_tag, "lastport")
                .and_then(|v| v.parse().ok())
                .filter(|&v| v > 0),
            last_login: extract_attr(char_tag, "lastlogin").unwrap_or_default(),
            desc,
        });
    }

    Ok(AccountInfo {
        account_id,
        email,
        characters,
    })
}

fn find_open_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let (start, end) = find_next_open_tag_offset(xml, tag)?;
    Some(&xml[start..=end])
}

fn find_next_open_tag_offset(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let needle = format!("<{tag}");
    let mut search_from = 0;
    while let Some(rel) = xml[search_from..].find(&needle) {
        let start = search_from + rel;
        let after_name = start + needle.len();
        let next_byte = xml.as_bytes().get(after_name).copied();
        let is_boundary = matches!(next_byte, Some(b) if b == b'>' || b == b'/' || b.is_ascii_whitespace());
        if is_boundary {
            let end_rel = xml[after_name..].find('>')?;
            return Some((start, after_name + end_rel));
        }
        search_from = after_name;
    }
    None
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pattern = format!("{attr}={quote}");
        let mut search_from = 0;
        while let Some(rel) = tag[search_from..].find(&pattern) {
            let start = search_from + rel;
            // Require preceding ASCII whitespace so `id="..."` never matches `userid="..."`.
            let boundary_ok = start > 0 && tag.as_bytes()[start - 1].is_ascii_whitespace();
            if boundary_ok {
                let val_start = start + pattern.len();
                if let Some(end) = tag[val_start..].find(quote) {
                    return Some(unescape_xml(&tag[val_start..val_start + end]));
                }
            }
            search_from = start + pattern.len();
        }
    }
    None
}

fn extract_tag_text(xml: &str, tag: &str) -> Option<String> {
    let (start, end) = find_next_open_tag_offset(xml, tag)?;
    let open_tag = &xml[start..=end];
    if open_tag.ends_with("/>") {
        return None;
    }
    let content_start = end + 1;
    let close = format!("</{tag}>");
    let close_rel = xml[content_start..].find(&close)?;
    Some(unescape_xml(xml[content_start..content_start + close_rel].trim()))
}

fn split_curl_http_status(stdout: &str) -> (&str, u16) {
    for marker in [HTTP_STATUS_MARKER, "__HTTP_STATUS__:"] {
        if let Some((body, code_str)) = stdout.rsplit_once(marker) {
            let code = code_str.trim().parse::<u16>().unwrap_or(0);
            return (body, code);
        }
    }
    (stdout, 0)
}

fn extract_notice_tag(xml: &str) -> Option<String> {
    ["msg", "message"]
        .into_iter()
        .find_map(|tag| extract_tag_text(xml, tag).filter(|s| !s.is_empty()))
}

fn http_400_message(body: &str) -> String {
    if let Some(msg) = extract_tag_text(body, "error")
        .filter(|s| !s.is_empty())
        .or_else(|| extract_notice_tag(body))
    {
        return msg;
    }
    let cleaned = clean_error_body(body);
    if cleaned.is_empty() {
        "HTTP 400 Bad Request".to_owned()
    } else {
        cleaned
    }
}

fn looks_like_html(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("<!doctype html") || lower.contains("<html")
}

fn clean_error_body(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let rest = &raw[i..];
        if rest.starts_with('<') {
            if let Some(gt) = rest.find('>') {
                if !rest[1..gt].contains('<') {
                    out.push(' ');
                    i += gt + 1;
                    continue;
                }
            }
        }
        if let Some(ch) = rest.chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    let unescaped = unescape_xml(&out);
    let collapsed = unescaped.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 300;
    if collapsed.chars().count() <= MAX_LEN {
        collapsed
    } else {
        let mut truncated: String = collapsed.chars().take(MAX_LEN).collect();
        truncated.push_str("...");
        truncated
    }
}

fn unescape_xml(s: &str) -> String {
    let mut decoded = s.to_owned();
    // Some account responses double-escape attribute values (for example
    // `&amp;gt;`); decode a few rounds so color codes are usable as sent.
    for _ in 0..3 {
        let next = decoded
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&#39;", "'")
            .replace("&amp;", "&");
        if next == decoded {
            break;
        }
        decoded = next;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_real_clogin_xml_response() {
        let sample = r#"<?xml version="1.0" encoding="utf-8"?>
<characterselect><account id="70805" email="player@example.com"><char id="43091366" name="Tactful" species="" colors="w6'(6&gt;'5/3(%3#(#" flags="0" created="2026-11-12 08:22:36" lastlogin="2026-11-12 20:22:55" lastitem="0" lastport="0" lastscale="100" lastgloam="0" lastspeci="0" messages="0" logins="1" time="75" owner="player@example.com"><desc>Hello &amp; welcome!</desc></char><char id="43091367" name="Tactful|Predator" lastlogin="2026-11-13 00:00:00"/></account></characterselect>"#;

        let info = parse_clogin_xml(sample).unwrap();
        assert_eq!(info.account_id, "70805");
        assert_eq!(info.email, "player@example.com");
        assert_eq!(info.characters.len(), 2);
        assert_eq!(info.characters[0].id, "43091366");
        assert_eq!(info.characters[0].name, "Tactful");
        assert_eq!(info.characters[0].desc, "Hello & welcome!");
        assert_eq!(info.characters[1].id, "43091367");
        assert_eq!(info.characters[1].name, "Tactful Predator");
    }

    #[test]
    fn extract_attr_respects_attribute_name_boundaries() {
        let tag = r#"<char userid="999" id="42" nickname="Wrong" name="Right"/>"#;
        assert_eq!(extract_attr(tag, "id").as_deref(), Some("42"));
        assert_eq!(extract_attr(tag, "name").as_deref(), Some("Right"));
    }

    #[test]
    fn parse_clogin_error_response() {
        let err_xml = r#"<?xml version="1.0"?><error>Invalid password</error>"#;
        assert!(matches!(
            parse_clogin_xml(err_xml),
            Err(AuthError::AuthFailed(msg)) if msg == "Invalid password"
        ));
    }

    #[test]
    fn parse_clogin_plain_text_and_attributed_error() {
        let plain = "Invalid email address or password.";
        assert!(matches!(
            parse_clogin_xml(plain),
            Err(AuthError::AuthFailed(msg)) if msg == "Invalid email address or password."
        ));

        let attributed = r#"<?xml version="1.0"?><error code="400">Account locked</error>"#;
        assert!(matches!(
            parse_clogin_xml(attributed),
            Err(AuthError::AuthFailed(msg)) if msg == "Account locked"
        ));

        let update_msg = r#"<?xml version="1.0"?><characterselect><update ver="31_2_final"><msg>Please update your client.</msg></update></characterselect>"#;
        assert!(matches!(
            parse_clogin_xml(update_msg),
            Err(AuthError::AuthFailed(msg)) if msg == "Please update your client."
        ));

        let empty_cs = r#"<?xml version="1.0"?><characterselect/>"#;
        assert!(matches!(
            parse_clogin_xml(empty_cs),
            Err(AuthError::InvalidResponse(msg)) if msg == "missing <account> element"
        ));
    }
}
