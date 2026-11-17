//! Furcadia rich chat markup parser and text/ANSI renderers.

/// Active formatting state for a text span in a Furcadia chat line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatStyle {
    pub color: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub link: Option<String>,
}

/// A single rendered segment inside a `ChatLine`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSpan {
    Text {
        text: String,
        style: ChatStyle,
    },
    Image {
        src: String,
        alt: Option<String>,
    },
}

/// Structured representation of a `(`-prefixed Furcadia chat/system message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub channel: Option<String>,
    pub spans: Vec<ChatSpan>,
}

impl ChatLine {
    /// Returns the plain-text representation of the chat line (using `alt` text for inline icons if non-empty).
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            match span {
                ChatSpan::Text { text, .. } => out.push_str(text),
                ChatSpan::Image { alt: Some(alt), .. } if !alt.is_empty() => {
                    out.push('[');
                    out.push_str(alt);
                    out.push(']');
                }
                ChatSpan::Image { .. } => {}
            }
        }
        out
    }

    /// Returns an ANSI-colored terminal representation of the chat line.
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            match span {
                ChatSpan::Image { alt: Some(alt), .. } if !alt.is_empty() => {
                    out.push_str("\x1b[2m[");
                    out.push_str(alt);
                    out.push_str("]\x1b[0m");
                }
                ChatSpan::Image { .. } => {}
                ChatSpan::Text { text, style } => {
                    let mut codes: Vec<&str> = Vec::new();
                    if style.bold {
                        codes.push("1");
                    }
                    if style.italic {
                        codes.push("3");
                    }
                    if style.underline || style.link.is_some() {
                        codes.push("4");
                    }
                    if let Some(color) = style.color.as_deref() {
                        if let Some(ansi) = map_color_to_ansi(color) {
                            codes.push(ansi);
                        }
                    } else if style.link.is_some() {
                        codes.push("36");
                    }

                    if codes.is_empty() {
                        out.push_str(text);
                    } else {
                        out.push_str("\x1b[");
                        out.push_str(&codes.join(";"));
                        out.push('m');
                        out.push_str(text);
                        out.push_str("\x1b[0m");
                    }
                }
            }
        }
        out
    }
}

fn map_color_to_ansi(color: &str) -> Option<&'static str> {
    match color.to_ascii_lowercase().as_str() {
        "success" | "green" => Some("32"),
        "warning" | "yellow" | "orange" => Some("33"),
        "error" | "danger" | "red" => Some("31"),
        "whisper" | "purple" | "magenta" | "trade" => Some("35"),
        "emit" | "bcast" | "broadcast" | "cyan" | "link" | "furclink" | "dragonspeak" => Some("36"),
        "shout" | "blue" | "notify" | "highlight" | "query" => Some("34"),
        "emote" => Some("3;35"),
        _ => None,
    }
}

/// Parses a Furcadia `(` chat payload (without the leading `(`) into a `ChatLine`.
pub fn parse_chat_line(input: &str) -> ChatLine {
    let mut channel = None;
    let mut spans = Vec::new();
    let mut style_stack: Vec<ChatStyle> = vec![ChatStyle::default()];
    let mut in_name_tag = 0usize;
    let mut rest = input;

    while !rest.is_empty() {
        if let Some(lt) = rest.find('<') {
            if lt > 0 {
                let decoded = unescape_html(&rest[..lt]);
                let clean = clean_furcadia_text(&decoded, in_name_tag > 0);
                push_text(&mut spans, &clean, style_stack.last().unwrap());
            }
            let after_lt = &rest[lt + 1..];
            if let Some(gt) = after_lt.find('>') {
                let raw_tag = after_lt[..gt].trim();
                rest = &after_lt[gt + 1..];
                handle_tag(raw_tag, &mut channel, &mut spans, &mut style_stack, &mut in_name_tag);
            } else {
                let decoded = unescape_html(&rest[lt..]);
                let clean = clean_furcadia_text(&decoded, in_name_tag > 0);
                push_text(&mut spans, &clean, style_stack.last().unwrap());
                break;
            }
        } else {
            let decoded = unescape_html(rest);
            let clean = clean_furcadia_text(&decoded, in_name_tag > 0);
            push_text(&mut spans, &clean, style_stack.last().unwrap());
            break;
        }
    }

    ChatLine { channel, spans }
}

fn push_text(spans: &mut Vec<ChatSpan>, text: &str, style: &ChatStyle) {
    if text.is_empty() {
        return;
    }
    if style.link.is_none() {
        let mut cursor = 0usize;
        let mut search = 0usize;
        while search < text.len() {
            let Some((start, prefix_len)) = find_plain_url(&text[search..]) else {
                break;
            };
            let start = search + start;
            let url_start = start;
            let mut end = url_start + prefix_len;
            while end < text.len() {
                let ch = text[end..].chars().next().unwrap();
                if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'') {
                    break;
                }
                end += ch.len_utf8();
            }
            let mut url_end = end;
            while url_end > url_start {
                let ch = text[..url_end].chars().next_back().unwrap();
                if matches!(ch, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}') {
                    url_end -= ch.len_utf8();
                } else {
                    break;
                }
            }
            if url_end == url_start {
                search = end.max(url_start + prefix_len);
                continue;
            }
            push_plain_text(spans, &text[cursor..url_start], style);
            let url = &text[url_start..url_end];
            let mut link_style = style.clone();
            link_style.link = Some(url.to_owned());
            push_plain_text(spans, url, &link_style);
            cursor = url_end;
            search = url_end;
        }
        if cursor == 0 {
            push_plain_text(spans, text, style);
        } else {
            push_plain_text(spans, &text[cursor..], style);
        }
        return;
    }
    push_plain_text(spans, text, style);
}

fn find_plain_url(text: &str) -> Option<(usize, usize)> {
    for (idx, _) in text.char_indices() {
        let prefix_len = if text[idx..].get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) {
            8
        } else if text[idx..].get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://")) {
            7
        } else {
            continue;
        };
        let boundary_ok = idx == 0
            || !text[..idx]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '@');
        if boundary_ok {
            return Some((idx, prefix_len));
        }
    }
    None
}

fn push_plain_text(spans: &mut Vec<ChatSpan>, text: &str, style: &ChatStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(ChatSpan::Text {
        text: prev,
        style: prev_style,
    }) = spans.last_mut()
    {
        if prev_style == style {
            prev.push_str(text);
            return;
        }
    }
    spans.push(ChatSpan::Text {
        text: text.to_owned(),
        style: style.clone(),
    });
}

fn handle_tag(
    raw_tag: &str,
    channel: &mut Option<String>,
    spans: &mut Vec<ChatSpan>,
    style_stack: &mut Vec<ChatStyle>,
    in_name_tag: &mut usize,
) {
    if raw_tag.is_empty() {
        return;
    }
    let self_closing = raw_tag.ends_with('/');
    let tag_body = raw_tag.trim_end_matches('/').trim();

    if let Some(closing) = tag_body.strip_prefix('/') {
        let name = closing.trim().to_ascii_lowercase();
        if name == "name" {
            *in_name_tag = in_name_tag.saturating_sub(1);
            if style_stack.len() > 1 {
                style_stack.pop();
            }
        } else if matches!(name.as_str(), "font" | "b" | "i" | "u" | "a" | "em" | "strong")
            && style_stack.len() > 1
        {
            style_stack.pop();
        }
        return;
    }

    let tag_name = tag_body
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    match tag_name.as_str() {
        "name" if !self_closing => {
            *in_name_tag += 1;
            let shortname = extract_attr_ci(tag_body, "shortname").unwrap_or_default();
            let mut next = style_stack.last().unwrap().clone();
            next.link = Some(format!("name://{shortname}"));
            if next.color.is_none() {
                next.color = Some("name".to_owned());
            }
            style_stack.push(next);
        }
        "channel" => {
            if let Some(name) = extract_attr_ci(tag_body, "name") {
                *channel = Some(name);
            }
        }
        "img" => {
            if let Some(src) = extract_attr_ci(tag_body, "src") {
                let alt = extract_attr_ci(tag_body, "alt");
                spans.push(ChatSpan::Image { src, alt });
            }
        }
        "font" if !self_closing => {
            let mut next = style_stack.last().unwrap().clone();
            if let Some(color) = extract_attr_ci(tag_body, "color") {
                if color.eq_ignore_ascii_case("emote") {
                    next.italic = true;
                }
                next.color = Some(color);
            }
            style_stack.push(next);
        }
        "b" | "strong" if !self_closing => {
            let mut next = style_stack.last().unwrap().clone();
            next.bold = true;
            style_stack.push(next);
        }
        "i" | "em" if !self_closing => {
            let mut next = style_stack.last().unwrap().clone();
            next.italic = true;
            style_stack.push(next);
        }
        "u" if !self_closing => {
            let mut next = style_stack.last().unwrap().clone();
            next.underline = true;
            style_stack.push(next);
        }
        "a" if !self_closing => {
            let mut next = style_stack.last().unwrap().clone();
            next.link = extract_attr_ci(tag_body, "href");
            style_stack.push(next);
        }
        "br" => {
            push_text(spans, "\n", style_stack.last().unwrap());
        }
        _ => {}
    }
}

fn extract_attr_ci(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    for quote in ['\'', '"'] {
        let pattern = format!("{attr}={quote}");
        if let Some(start) = lower.find(&pattern) {
            let val_start = start + pattern.len();
            if let Some(end) = tag[val_start..].find(quote) {
                return Some(unescape_html(&tag[val_start..val_start + end]));
            }
        }
    }
    None
}

fn clean_furcadia_text(text: &str, in_name_tag: bool) -> String {
    if in_name_tag
        || text.starts_with("(You see ")
        || text.starts_with("You see ")
        || (text.starts_with("> ") && text.contains('|'))
    {
        return text.replace('|', " ");
    }
    if let Some(idx) = text.find(" whispers,") {
        let mut out = text[..idx].replace('|', " ");
        out.push_str(&text[idx..]);
        return out;
    }
    text.to_owned()
}

/// Decodes common HTML character references and numeric entities (`&#...;`, `&#x...;`).
pub fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after_amp = &rest[amp..];
        if let Some(semi) = after_amp[1..].find(';').filter(|&len| len <= 10) {
            let entity = &after_amp[1..1 + semi];
            let replacement = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "amp" => Some('&'),
                "nbsp" => Some(' '),
                "euro" => Some('€'),
                "trade" => Some('™'),
                "middot" | "sblt" => Some('·'),
                "bblt" => Some('•'),
                "copy" => Some('©'),
                "reg" => Some('®'),
                "deg" => Some('°'),
                "laquo" => Some('«'),
                "raquo" => Some('»'),
                "times" => Some('×'),
                "divide" => Some('÷'),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = replacement {
                out.push(ch);
                rest = &after_amp[2 + semi..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[amp + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_live_event_channel_line() {
        let raw = "<font color='success'><img src='fsh://system.fsh:83' alt='@events' /><channel name='@events' /> Event channel on.</font>";
        let parsed = parse_chat_line(raw);
        assert_eq!(parsed.channel.as_deref(), Some("@events"));
        assert_eq!(parsed.to_plain_text(), "[@events] Event channel on.");
    }

    #[test]
    fn parse_warning_with_bold_italic_and_link() {
        let raw = "<font color='warning'>Warning: <B>13</B> days, type <I>setemail &lt;email&gt;</I>. <A HREF='http://www.furcadia.com/email.html'>Click here</A></font>";
        let parsed = parse_chat_line(raw);
        assert_eq!(
            parsed.to_plain_text(),
            "Warning: 13 days, type setemail <email>. Click here"
        );
        assert!(parsed.spans.iter().any(|s| matches!(
            s,
            ChatSpan::Text { text, style } if text == "Click here" && style.link.as_deref() == Some("http://www.furcadia.com/email.html")
        )));
    }

    #[test]
    fn parse_emote_name_pipes_and_entities() {
        let raw = "<font color='emote'><name shortname='riza|lightning'>Riza|Lightning</name> waves&#33; &#x27;hi&#x27;</font>";
        let parsed = parse_chat_line(raw);
        assert_eq!(parsed.to_plain_text(), "Riza Lightning waves! 'hi'");
        assert!(parsed.spans.iter().all(|s| matches!(
            s,
            ChatSpan::Text { style, .. } if style.italic && style.color.as_deref() == Some("emote")
        )));
        assert!(parsed.spans.iter().any(|s| matches!(
            s,
            ChatSpan::Text { text, style }
                if text == "Riza Lightning" && style.link.as_deref() == Some("name://riza|lightning")
        )));

        let raw_name = "<name shortname='aleri'>Alerius</name>: Hello!";
        let parsed_name = parse_chat_line(raw_name);
        assert!(parsed_name.spans.iter().any(|s| matches!(
            s,
            ChatSpan::Text { text, style }
                if text == "Alerius"
                    && style.link.as_deref() == Some("name://aleri")
                    && style.color.as_deref() == Some("name")
        )));

        let see = parse_chat_line("(You see Riza|Lightning.)");
        assert_eq!(see.to_plain_text(), "(You see Riza Lightning.)");

        let no_double = parse_chat_line("&amp;#39;");
        assert_eq!(no_double.to_plain_text(), "&#39;");
    }
}
