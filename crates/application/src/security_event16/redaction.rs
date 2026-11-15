//! Credential redaction for the free-form `techInfo` of an OCPP 1.6 security event.
//!
//! The text is charger-controlled and may carry passwords, `AuthorizationKey` values, tokens or
//! URLs with credentials. Four deterministic rules run in order and replace each secret with
//! [`REDACTION_MARKER`], keeping the surrounding context:
//!
//! 1. the value after a secret label (see [`label`]), an `Authorization` header as a whole;
//! 2. the token after a `Basic` or `Bearer` scheme word;
//! 3. URL userinfo between `://` and `@`;
//! 4. any long token-character run that looks like a hex or mixed-case base64 secret.
//!
//! Redacting redacted text changes nothing: a value that is exactly the marker is never matched
//! again, and the marker itself contains no label or token run.
mod label;

use uob_contracts::SECURITY_EVENT_TECH_INFO_MAX_CHARS_16;

/// Replaces every redacted value.
pub const REDACTION_MARKER: &str = "[redacted]";

const SCHEMES: [&str; 2] = ["basic", "bearer"];

/// An unquoted value ends at whitespace or one of these. Quotes do not end it, so a password
/// that contains one is still removed completely.
const VALUE_STOPS: &str = ",;&})";
/// A scheme token ends at whitespace, these, or a quote.
const TOKEN_STOPS: &str = ",;&})\"'";
/// Characters that end URL userinfo before an `@` can be found.
const USERINFO_STOPS: &str = "/?#\"'<>";

const MIN_HEX_SECRET_CHARS: usize = 32;
const MIN_BASE64_SECRET_CHARS: usize = 24;

/// The text kept for a security event and how many values were replaced to produce it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedTechInfo16 {
    pub text: String,
    pub redactions: u16,
}

/// A rule finds, at one position, the span `start..end` (with `start >= position`) to replace.
type Rule = fn(&[char], usize) -> Option<(usize, usize)>;

const RULES: [Rule; 4] = [
    label::labelled_value,
    scheme_token,
    url_userinfo,
    long_token,
];

/// Removes credential-like values from `raw`. Pure and deterministic; the output is at most
/// 1024 characters.
#[must_use]
pub fn redact_tech_info_16(raw: &str) -> RedactedTechInfo16 {
    let mut chars: Vec<char> = raw.chars().collect();
    let mut redactions = 0_u16;

    for rule in RULES {
        let (next, count) = replace_spans(&chars, rule);
        chars = next;
        redactions = redactions.saturating_add(count);
    }
    chars.truncate(SECURITY_EVENT_TECH_INFO_MAX_CHARS_16);

    RedactedTechInfo16 {
        text: chars.into_iter().collect(),
        redactions,
    }
}

fn replace_spans(chars: &[char], rule: Rule) -> (Vec<char>, u16) {
    let marker: Vec<char> = REDACTION_MARKER.chars().collect();
    let mut output = Vec::with_capacity(chars.len());
    let mut copied = 0;
    let mut count = 0_u16;
    let mut position = 0;

    while position < chars.len() {
        let Some((start, end)) = rule(chars, position) else {
            position += 1;
            continue;
        };

        output.extend_from_slice(&chars[copied..start]);
        output.extend_from_slice(&marker);
        copied = end;
        position = end;
        count = count.saturating_add(1);
    }
    output.extend_from_slice(&chars[copied..]);

    (output, count)
}

/// Rule 2: the token after a `Basic` or `Bearer` word.
fn scheme_token(chars: &[char], position: usize) -> Option<(usize, usize)> {
    if position > 0 && chars[position - 1].is_alphanumeric() {
        return None;
    }
    let word_end = SCHEMES
        .iter()
        .find_map(|scheme| word_at(chars, position, scheme))?;

    scheme_token_after(chars, word_end)
}

/// The token that follows a scheme word ending at `word_end`, after at least one whitespace.
fn scheme_token_after(chars: &[char], word_end: usize) -> Option<(usize, usize)> {
    let start = skip_whitespace(chars, word_end);
    if start == word_end {
        return None;
    }
    let end = value_end(chars, start, TOKEN_STOPS);

    span(chars, start, end)
}

/// A labelled value that is only a scheme word (`token: Bearer abc`) also takes its token.
fn include_scheme_token(chars: &[char], start: usize, end: usize) -> usize {
    let is_scheme = SCHEMES
        .iter()
        .any(|scheme| word_at(chars, start, scheme) == Some(end));
    if !is_scheme {
        return end;
    }

    scheme_token_after(chars, end).map_or(end, |(_, token_end)| token_end)
}

/// Rule 3: the userinfo of the URL authority that follows `://`.
fn url_userinfo(chars: &[char], position: usize) -> Option<(usize, usize)> {
    if !chars[position..].starts_with(&[':', '/', '/']) {
        return None;
    }
    let start = position + 3;
    let mut at = None;

    for (offset, c) in chars[start..].iter().enumerate() {
        if c.is_whitespace() || USERINFO_STOPS.contains(*c) {
            break;
        }
        if *c == '@' {
            at = Some(start + offset);
        }
    }

    span(chars, start, at?)
}

/// Rule 4: a maximal token-character run (with its trailing `=` padding) that is a long hex string, or a long base64 string
/// mixing upper case, lower case and digits.
fn long_token(chars: &[char], position: usize) -> Option<(usize, usize)> {
    if position > 0 && is_token_char(chars[position - 1]) {
        return None;
    }
    let body = chars[position..]
        .iter()
        .take_while(|c| is_token_char(**c))
        .count();
    let padding = chars[position + body..]
        .iter()
        .take_while(|c| **c == '=')
        .count();
    let end = position + body + padding;
    let run = &chars[position..end];

    let hex = run.len() >= MIN_HEX_SECRET_CHARS && run.iter().all(char::is_ascii_hexdigit);
    let base64 = run.len() >= MIN_BASE64_SECRET_CHARS
        && run.iter().any(char::is_ascii_uppercase)
        && run.iter().any(char::is_ascii_lowercase)
        && run.iter().any(char::is_ascii_digit);

    if hex || base64 {
        span(chars, position, end)
    } else {
        None
    }
}

/// The end of `word` matched case-insensitively at `position`, or `None`.
fn word_at(chars: &[char], position: usize, word: &str) -> Option<usize> {
    let mut cursor = position;

    for expected in word.chars() {
        if !chars.get(cursor)?.eq_ignore_ascii_case(&expected) {
            return None;
        }
        cursor += 1;
    }

    Some(cursor)
}

fn value_end(chars: &[char], start: usize, stops: &str) -> usize {
    start
        + chars[start..]
            .iter()
            .take_while(|c| !c.is_whitespace() && !stops.contains(**c))
            .count()
}

fn skip_whitespace(chars: &[char], start: usize) -> usize {
    start
        + chars[start..]
            .iter()
            .take_while(|c| c.is_whitespace())
            .count()
}

/// A non-empty span that is not already exactly the marker.
fn span(chars: &[char], start: usize, end: usize) -> Option<(usize, usize)> {
    if end <= start || is_marker(&chars[start..end]) {
        return None;
    }

    Some((start, end))
}

fn is_marker(text: &[char]) -> bool {
    text.len() == REDACTION_MARKER.chars().count()
        && text
            .iter()
            .zip(REDACTION_MARKER.chars())
            .all(|(a, b)| *a == b)
}

const fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'')
}

const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_')
}
