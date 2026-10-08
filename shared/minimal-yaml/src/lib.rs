//! A zero-copy parser for a strict, mapping-only YAML subset.
//!
//! Direct mapping entries are discovered eagerly, but their values are parsed
//! only when [`Entry::scalar`] or [`Entry::mapping`] is called. Callers can
//! therefore ignore an unknown entry without parsing or allocating its subtree.

#![no_std]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::string::String;
use core::fmt;

/// Begins parsing a block mapping from `input`.
#[must_use]
pub const fn mapping(input: &str) -> Mapping<'_> {
    Mapping {
        input,
        indent: 0,
        first_line: 1,
    }
}

/// One block mapping backed by the original YAML text.
#[derive(Clone, Copy, Debug)]
pub struct Mapping<'a> {
    input: &'a str,
    indent: usize,
    first_line: usize,
}

impl<'a> Mapping<'a> {
    /// Iterates over direct entries in this mapping.
    #[must_use]
    pub const fn entries(&self) -> Entries<'a> {
        Entries {
            input: self.input,
            indent: self.indent,
            offset: 0,
            line: self.first_line,
        }
    }
}

/// Direct-entry iterator for a [`Mapping`].
pub struct Entries<'a> {
    input: &'a str,
    indent: usize,
    offset: usize,
    line: usize,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<Entry<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = line_at(self.input, self.offset, self.line)?;
            self.offset = line.next_offset;
            self.line = line.number.saturating_add(1);

            let (indent, content) = match split_indent(line.text, line.number) {
                Ok(parts) => parts,
                Err(error) => return Some(Err(error)),
            };
            if is_ignorable(content) {
                continue;
            }
            if indent != self.indent {
                return Some(Err(Error::new(line.number, ErrorKind::UnexpectedIndent)));
            }

            let Some((raw_key, raw_value)) = content.split_once(':') else {
                return Some(Err(Error::new(
                    line.number,
                    ErrorKind::ExpectedMappingEntry,
                )));
            };
            let key = raw_key.trim();
            if !valid_key(key) {
                return Some(Err(Error::new(line.number, ErrorKind::InvalidKey)));
            }

            let child_start = self.offset;
            let child_first_line = self.line;
            let mut scan_offset = self.offset;
            let mut scan_line = self.line;
            while let Some(candidate) = line_at(self.input, scan_offset, scan_line) {
                let (candidate_indent, candidate_content) = boundary_parts(candidate.text);
                if !candidate_content.trim().is_empty() && candidate_indent <= self.indent {
                    break;
                }
                scan_offset = candidate.next_offset;
                scan_line = candidate.number.saturating_add(1);
            }

            let Some(child) = self.input.get(child_start..scan_offset) else {
                return Some(Err(Error::new(line.number, ErrorKind::InvalidRange)));
            };
            self.offset = scan_offset;
            self.line = scan_line;

            return Some(Ok(Entry {
                key,
                line: line.number,
                parent_indent: self.indent,
                raw_value: raw_value.trim(),
                child,
                child_first_line,
            }));
        }
    }
}

/// One mapping entry whose value is parsed only when requested.
#[derive(Debug)]
pub struct Entry<'a> {
    key: &'a str,
    line: usize,
    parent_indent: usize,
    raw_value: &'a str,
    child: &'a str,
    child_first_line: usize,
}

impl<'a> Entry<'a> {
    /// Returns the borrowed mapping key.
    #[must_use]
    pub const fn key(&self) -> &'a str {
        self.key
    }

    /// Reads this entry as a string scalar.
    ///
    /// Plain and unescaped quoted values borrow the input. Decoding an escape
    /// or rendering a block scalar returns an owned string.
    pub fn scalar(&self) -> Result<Cow<'a, str>, Error> {
        match self.raw_value {
            ">" => render_block(
                self.child,
                self.parent_indent,
                self.child_first_line,
                BlockStyle::Folded,
            ),
            "|" => render_block(
                self.child,
                self.parent_indent,
                self.child_first_line,
                BlockStyle::Literal,
            ),
            "" if has_data_line(self.child) => {
                Err(Error::new(self.line, ErrorKind::ExpectedScalar))
            }
            "" => Ok(Cow::Borrowed("")),
            _ if has_data_line(self.child) => {
                Err(Error::new(self.line, ErrorKind::UnexpectedNestedValue))
            }
            value => parse_inline_scalar(value, self.line),
        }
    }

    /// Reads this entry as a nested block mapping.
    pub fn mapping(&self) -> Result<Mapping<'a>, Error> {
        if !self.raw_value.is_empty() {
            return Err(Error::new(self.line, ErrorKind::ExpectedMapping));
        }
        let Some(indent) =
            first_mapping_indent(self.child, self.parent_indent, self.child_first_line)?
        else {
            return Err(Error::new(self.line, ErrorKind::ExpectedMapping));
        };
        Ok(Mapping {
            input: self.child,
            indent,
            first_line: self.child_first_line,
        })
    }
}

#[derive(Clone, Copy)]
enum BlockStyle {
    Folded,
    Literal,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    next_offset: usize,
    number: usize,
}

fn line_at(input: &str, offset: usize, number: usize) -> Option<Line<'_>> {
    let remaining = input.get(offset..)?;
    if remaining.is_empty() {
        return None;
    }
    let (text, next_offset) = match remaining.find('\n') {
        Some(end) => (
            remaining.get(..end)?,
            offset.saturating_add(end).saturating_add(1),
        ),
        None => (remaining, input.len()),
    };
    Some(Line {
        text: text.strip_suffix('\r').unwrap_or(text),
        next_offset,
        number,
    })
}

fn split_indent(text: &str, line: usize) -> Result<(usize, &str), Error> {
    let mut indent = 0usize;
    for byte in text.bytes() {
        match byte {
            b' ' => indent = indent.saturating_add(1),
            b'\t' => return Err(Error::new(line, ErrorKind::TabIndentation)),
            _ => break,
        }
    }
    let content = text
        .get(indent..)
        .ok_or_else(|| Error::new(line, ErrorKind::InvalidRange))?;
    Ok((indent, content))
}

fn boundary_parts(text: &str) -> (usize, &str) {
    let mut indent = 0usize;
    for byte in text.bytes() {
        match byte {
            b' ' | b'\t' => indent = indent.saturating_add(1),
            _ => break,
        }
    }
    (indent, text.get(indent..).unwrap_or(""))
}

fn is_ignorable(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn has_data_line(input: &str) -> bool {
    let mut offset = 0usize;
    let mut line = 1usize;
    while let Some(candidate) = line_at(input, offset, line) {
        let (_, content) = boundary_parts(candidate.text);
        if !is_ignorable(content) {
            return true;
        }
        offset = candidate.next_offset;
        line = line.saturating_add(1);
    }
    false
}

fn first_mapping_indent(
    input: &str,
    parent_indent: usize,
    first_line: usize,
) -> Result<Option<usize>, Error> {
    let mut offset = 0usize;
    let mut line = first_line;
    while let Some(candidate) = line_at(input, offset, line) {
        let (indent, content) = split_indent(candidate.text, candidate.number)?;
        if !is_ignorable(content) {
            if indent <= parent_indent {
                return Err(Error::new(candidate.number, ErrorKind::UnexpectedIndent));
            }
            return Ok(Some(indent));
        }
        offset = candidate.next_offset;
        line = candidate.number.saturating_add(1);
    }
    Ok(None)
}

fn parse_inline_scalar(value: &str, line: usize) -> Result<Cow<'_, str>, Error> {
    if value.starts_with('"') {
        return parse_double_quoted(value, line);
    }
    if value.starts_with('\'') {
        return parse_single_quoted(value, line);
    }
    if value.starts_with('[')
        || value.starts_with('{')
        || value.starts_with('&')
        || value.starts_with('*')
        || value.starts_with('!')
        || value == "-"
        || value.starts_with("- ")
    {
        return Err(Error::new(line, ErrorKind::UnsupportedValue));
    }
    Ok(Cow::Borrowed(value))
}

fn parse_double_quoted(value: &str, line: usize) -> Result<Cow<'_, str>, Error> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
    else {
        return Err(Error::new(line, ErrorKind::UnterminatedQuotedScalar));
    };
    if !inner.contains('\\') && !inner.contains('"') {
        return Ok(Cow::Borrowed(inner));
    }

    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next() {
                Some('"') => decoded.push('"'),
                Some('\\') => decoded.push('\\'),
                Some('n') => decoded.push('\n'),
                Some('r') => decoded.push('\r'),
                Some('t') => decoded.push('\t'),
                _ => return Err(Error::new(line, ErrorKind::InvalidEscape)),
            },
            '"' => return Err(Error::new(line, ErrorKind::InvalidEscape)),
            other => decoded.push(other),
        }
    }
    Ok(Cow::Owned(decoded))
}

fn parse_single_quoted(value: &str, line: usize) -> Result<Cow<'_, str>, Error> {
    let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\''))
    else {
        return Err(Error::new(line, ErrorKind::UnterminatedQuotedScalar));
    };
    if !inner.contains('\'') {
        return Ok(Cow::Borrowed(inner));
    }

    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\'' {
            decoded.push(character);
            continue;
        }
        if chars.next() != Some('\'') {
            return Err(Error::new(line, ErrorKind::InvalidEscape));
        }
        decoded.push('\'');
    }
    Ok(Cow::Owned(decoded))
}

fn render_block(
    input: &str,
    parent_indent: usize,
    first_line: usize,
    style: BlockStyle,
) -> Result<Cow<'_, str>, Error> {
    let Some(indent) = block_indent(input, parent_indent, first_line)? else {
        return Ok(Cow::Owned(String::new()));
    };
    let mut rendered = String::new();
    let mut offset = 0usize;
    let mut line = first_line;
    let mut has_text = false;
    let mut blank_lines = 0usize;

    while let Some(candidate) = line_at(input, offset, line) {
        let (line_indent, content) = split_indent(candidate.text, candidate.number)?;
        if content.is_empty() {
            blank_lines = blank_lines.saturating_add(1);
        } else {
            if line_indent < indent {
                return Err(Error::new(candidate.number, ErrorKind::UnexpectedIndent));
            }
            let value = candidate
                .text
                .get(indent..)
                .ok_or_else(|| Error::new(candidate.number, ErrorKind::InvalidRange))?;
            match style {
                BlockStyle::Literal => {
                    while blank_lines > 0 {
                        rendered.push('\n');
                        blank_lines = blank_lines.saturating_sub(1);
                    }
                    rendered.push_str(value);
                    rendered.push('\n');
                }
                BlockStyle::Folded => {
                    if has_text {
                        if blank_lines == 0 {
                            rendered.push(' ');
                        } else {
                            while blank_lines > 0 {
                                rendered.push('\n');
                                blank_lines = blank_lines.saturating_sub(1);
                            }
                        }
                    }
                    rendered.push_str(value);
                    has_text = true;
                }
            }
        }
        offset = candidate.next_offset;
        line = candidate.number.saturating_add(1);
    }

    if matches!(style, BlockStyle::Folded) && has_text {
        rendered.push('\n');
    }
    Ok(Cow::Owned(rendered))
}

fn block_indent(
    input: &str,
    parent_indent: usize,
    first_line: usize,
) -> Result<Option<usize>, Error> {
    // YAML detects a block scalar's indentation from its first non-empty
    // line; a later, less indented content line is an error, reported by
    // `render_block`.
    let mut offset = 0usize;
    let mut line = first_line;
    while let Some(candidate) = line_at(input, offset, line) {
        let (indent, content) = split_indent(candidate.text, candidate.number)?;
        if !content.is_empty() {
            if indent <= parent_indent {
                return Err(Error::new(candidate.number, ErrorKind::UnexpectedIndent));
            }
            return Ok(Some(indent));
        }
        offset = candidate.next_offset;
        line = candidate.number.saturating_add(1);
    }
    Ok(None)
}

/// A syntax or requested-value mismatch in the supported YAML subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    line: usize,
    kind: ErrorKind,
}

impl Error {
    const fn new(line: usize, kind: ErrorKind) -> Self {
        Self { line, kind }
    }

    /// Returns the one-based source line containing the error.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "minimal YAML error at line {}: {}",
            self.line, self.kind
        )
    }
}

impl core::error::Error for Error {}

/// Stable categories reported by the strict subset parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// A line is indented where a direct mapping entry is required.
    UnexpectedIndent,
    /// A non-empty line does not contain a `key: value` entry.
    ExpectedMappingEntry,
    /// A mapping key is empty or contains unsupported characters.
    InvalidKey,
    /// Tab indentation is unsupported.
    TabIndentation,
    /// A requested value is not a scalar.
    ExpectedScalar,
    /// A requested value is not a nested mapping.
    ExpectedMapping,
    /// A known value uses an unsupported YAML construct.
    UnsupportedValue,
    /// A quoted scalar is missing its closing quote.
    UnterminatedQuotedScalar,
    /// A quoted scalar contains an unsupported or malformed escape.
    InvalidEscape,
    /// An inline scalar unexpectedly also owns an indented subtree.
    UnexpectedNestedValue,
    /// An internal source range could not be represented safely.
    InvalidRange,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnexpectedIndent => "unexpected indentation",
            Self::ExpectedMappingEntry => "expected a mapping entry",
            Self::InvalidKey => "invalid mapping key",
            Self::TabIndentation => "tab indentation is unsupported",
            Self::ExpectedScalar => "expected a scalar value",
            Self::ExpectedMapping => "expected a nested mapping",
            Self::UnsupportedValue => "unsupported value syntax",
            Self::UnterminatedQuotedScalar => "unterminated quoted scalar",
            Self::InvalidEscape => "invalid quoted-scalar escape",
            Self::UnexpectedNestedValue => "inline scalar has a nested value",
            Self::InvalidRange => "invalid source range",
        })
    }
}
