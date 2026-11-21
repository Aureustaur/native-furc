//! Furcadia DragonSpeak (`.ds`) script parser, serializer, and trigger model.
//!
//! DragonSpeak scripts consist of trigger blocks beginning with a `(0:id)` cause line,
//! followed by optional `(1:id)` conditions, `(3:id)` area selectors, `(4:id)` filters,
//! and `(5:id)` effects. Lines may reference integer literals, `%variable` names, or
//! `{string}` literals.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// DragonSpeak line category (`(N:id)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DsCategory {
    Cause,
    Condition,
    Area,
    Filter,
    Effect,
    Unknown(u8),
}

impl DsCategory {
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Cause,
            1 => Self::Condition,
            3 => Self::Area,
            4 => Self::Filter,
            5 => Self::Effect,
            other => Self::Unknown(other),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Cause => 0,
            Self::Condition => 1,
            Self::Area => 3,
            Self::Filter => 4,
            Self::Effect => 5,
            Self::Unknown(c) => c,
        }
    }
}

/// An argument on a DragonSpeak line (number, `{text}`/`[text]` literal, or `%var`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DsArg {
    Number(i64),
    Text(String),
    Variable(String),
}

impl fmt::Display for DsArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(t) => write!(f, "{{{t}}}"),
            Self::Variable(v) => write!(f, "%{v}"),
        }
    }
}

/// A single parsed DragonSpeak instruction line, e.g. `(5:15) 12,30`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsLine {
    pub category: DsCategory,
    pub id: u16,
    pub args: Vec<DsArg>,
    pub line_number: usize,
    pub raw: String,
}

impl DsLine {
    /// Formats the instruction in canonical `.ds` syntax.
    pub fn to_ds_string(&self) -> String {
        let indent = match self.category {
            DsCategory::Cause => "",
            _ => "\t",
        };
        if self.args.is_empty() {
            format!("{indent}({}:{})", self.category.code(), self.id)
        } else {
            let args_str: Vec<String> = self.args.iter().map(ToString::to_string).collect();
            format!(
                "{indent}({}:{}) {}",
                self.category.code(),
                self.id,
                args_str.join(",")
            )
        }
    }
}

/// A complete DragonSpeak trigger block starting with one or more causes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsTrigger {
    pub causes: Vec<DsLine>,
    pub conditions: Vec<DsLine>,
    pub areas_and_filters: Vec<DsLine>,
    pub effects: Vec<DsLine>,
}

/// Parsed `.ds` DragonSpeak script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsScript {
    pub version: String,
    pub triggers: Vec<DsTrigger>,
    pub comments: Vec<String>,
}

/// Errors returned when loading or parsing a `.ds` script.
#[derive(Debug)]
pub enum DsError {
    Io(io::Error),
    InvalidLine { line_number: usize, content: String },
}

impl fmt::Display for DsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidLine {
                line_number,
                content,
            } => write!(f, "invalid DragonSpeak at line {line_number}: {content}"),
        }
    }
}

impl std::error::Error for DsError {}

impl From<io::Error> for DsError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl DsScript {
    /// Loads and parses a `.ds` file from disk (Latin-1 / UTF-8 lossy safe).
    pub fn load(path: &Path) -> Result<Self, DsError> {
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        Self::parse(&text)
    }

    /// Parses a `.ds` script from text.
    pub fn parse(input: &str) -> Result<Self, DsError> {
        let mut version = String::from("DSPK V04.00 Furcadia");
        let mut triggers: Vec<DsTrigger> = Vec::new();
        let mut comments: Vec<String> = Vec::new();
        let mut current: Option<DsTrigger> = None;

        for (idx, raw_line) in input.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("DSPK ") {
                version = trimmed.to_owned();
                continue;
            }
            if trimmed.starts_with("*Endtriggers*") {
                break;
            }
            if trimmed.starts_with('*') {
                comments.push(trimmed.trim_matches('*').trim().to_owned());
                continue;
            }

            if trimmed.starts_with('(') {
                let ds_line = parse_ds_line(trimmed, line_number)?;
                push_ds_line(&mut triggers, &mut current, ds_line);
            } else if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
                && trimmed.contains(':')
            {
                for ds_line in parse_legacy_inline_lines(trimmed, line_number) {
                    push_ds_line(&mut triggers, &mut current, ds_line);
                }
            }
        }

        if let Some(last) = current {
            triggers.push(last);
        }

        Ok(Self {
            version,
            triggers,
            comments,
        })
    }

    /// Serializes the script back into standard `.ds` format.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.version);
        out.push('\n');
        for trig in &self.triggers {
            for c in &trig.causes {
                out.push_str(&c.to_ds_string());
                out.push('\n');
            }
            for c in &trig.conditions {
                out.push_str(&c.to_ds_string());
                out.push('\n');
            }
            for af in &trig.areas_and_filters {
                out.push_str(&af.to_ds_string());
                out.push('\n');
            }
            for e in &trig.effects {
                out.push_str(&e.to_ds_string());
                out.push('\n');
            }
        }
        out.push_str("*Endtriggers* 9999 *Endtriggers*\n");
        out
    }
}

fn push_ds_line(
    triggers: &mut Vec<DsTrigger>,
    current: &mut Option<DsTrigger>,
    ds_line: DsLine,
) {
    match ds_line.category {
        DsCategory::Cause => {
            if let Some(prev) = current.as_mut() {
                if !prev.conditions.is_empty()
                    || !prev.areas_and_filters.is_empty()
                    || !prev.effects.is_empty()
                {
                    triggers.push(current.take().unwrap());
                }
            }
            current
                .get_or_insert_with(DsTrigger::default)
                .causes
                .push(ds_line);
        }
        DsCategory::Condition => {
            current
                .get_or_insert_with(DsTrigger::default)
                .conditions
                .push(ds_line);
        }
        DsCategory::Area | DsCategory::Filter => {
            current
                .get_or_insert_with(DsTrigger::default)
                .areas_and_filters
                .push(ds_line);
        }
        DsCategory::Effect | DsCategory::Unknown(_) => {
            current
                .get_or_insert_with(DsTrigger::default)
                .effects
                .push(ds_line);
        }
    }
}

fn parse_legacy_inline_lines(trimmed: &str, line_number: usize) -> Vec<DsLine> {
    let mut out: Vec<DsLine> = Vec::new();
    for token in trimmed.split_whitespace() {
        if let Some((c, i)) = token.split_once(':') {
            if let (Ok(cat), Ok(id)) = (c.parse::<u8>(), i.parse::<u16>()) {
                out.push(DsLine {
                    category: DsCategory::from_code(cat),
                    id,
                    args: Vec::new(),
                    line_number,
                    raw: token.to_owned(),
                });
                continue;
            }
        }
        if let Some(last) = out.last_mut() {
            if let Ok(n) = token.parse::<i64>() {
                last.args.push(DsArg::Number(n));
            } else {
                last.args.push(DsArg::Text(token.to_owned()));
            }
        }
    }
    out
}

fn parse_ds_line(trimmed: &str, line_number: usize) -> Result<DsLine, DsError> {
    let close_paren = trimmed.find(')').ok_or_else(|| DsError::InvalidLine {
        line_number,
        content: trimmed.to_owned(),
    })?;
    let header = &trimmed[1..close_paren];
    let (cat_str, id_str) = header.split_once(':').ok_or_else(|| DsError::InvalidLine {
        line_number,
        content: trimmed.to_owned(),
    })?;

    let cat_code: u8 = cat_str.trim().parse().map_err(|_| DsError::InvalidLine {
        line_number,
        content: trimmed.to_owned(),
    })?;
    let id: u16 = id_str.trim().parse().map_err(|_| DsError::InvalidLine {
        line_number,
        content: trimmed.to_owned(),
    })?;

    let rest = trimmed[close_paren + 1..].trim();
    let args = parse_ds_args(rest);

    Ok(DsLine {
        category: DsCategory::from_code(cat_code),
        id,
        args,
        line_number,
        raw: trimmed.to_owned(),
    })
}

fn parse_ds_args(rest: &str) -> Vec<DsArg> {
    if rest.is_empty() {
        return Vec::new();
    }

    let mut args = Vec::new();
    let mut chars = rest.char_indices().peekable();

    while let Some(&(start, ch)) = chars.peek() {
        if ch.is_whitespace() || ch == ',' {
            chars.next();
            continue;
        }
        if ch == '{' || ch == '[' {
            let close = if ch == '{' { '}' } else { ']' };
            chars.next();
            let text_start = start + ch.len_utf8();
            let mut text_end = rest.len();
            for (pos, c) in chars.by_ref() {
                if c == close {
                    text_end = pos;
                    break;
                }
            }
            args.push(DsArg::Text(rest[text_start..text_end].to_owned()));
        } else {
            let mut token_end = rest.len();
            while let Some(&(pos, c)) = chars.peek() {
                if c == ',' || c.is_whitespace() {
                    token_end = pos;
                    break;
                }
                chars.next();
            }
            let token = &rest[start..token_end];
            if let Some(var_name) = token.strip_prefix('%') {
                args.push(DsArg::Variable(var_name.to_owned()));
            } else if let Ok(num) = token.parse::<i64>() {
                args.push(DsArg::Number(num));
            } else if !token.is_empty() {
                args.push(DsArg::Text(token.to_owned()));
            }
        }
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetLocator;
    use std::fs;

    #[test]
    fn parse_and_roundtrip_ds_script() {
        let sample = "\
DSPK V04.00 Furcadia
*Sample trigger*
(0:9)
\t(1:10) 5
\t(5:200) {Hello, world!},%counter
*Endtriggers* 9999 *Endtriggers*
";
        let script = DsScript::parse(sample).unwrap();
        assert_eq!(script.version, "DSPK V04.00 Furcadia");
        assert_eq!(script.triggers.len(), 1);
        let trig = &script.triggers[0];
        assert_eq!(trig.causes.len(), 1);
        assert_eq!(trig.conditions.len(), 1);
        assert_eq!(
            trig.effects[0].args,
            vec![
                DsArg::Text("Hello, world!".into()),
                DsArg::Variable("counter".into())
            ]
        );

        let re_parsed = DsScript::parse(&script.serialize()).unwrap();
        assert_eq!(re_parsed.triggers.len(), 1);
    }

    #[test]
    fn parse_workspace_map_ds_files_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let maps_dir = root.join("maps");
        if !maps_dir.is_dir() {
            return;
        }
        let mut count = 0;
        for entry in fs::read_dir(&maps_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("ds") {
                let script = DsScript::load(&path).unwrap();
                assert!(!script.triggers.is_empty(), "empty triggers in {path:?}");
                count += 1;
            }
        }
        assert!(count > 0);
    }
}
