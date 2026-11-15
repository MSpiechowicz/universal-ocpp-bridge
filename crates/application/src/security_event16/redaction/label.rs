//! Rule 1: the value after a secret label.
//!
//! A label is the end of the identifier (letters, digits, `_`, `-`, `.`) that directly precedes
//! the separator, so `dbPassword=`, `x_secret:` and `wifi.pwd =` all count. A trailing plural or
//! number is ignored (`tokens=`, `password1=`). The short labels `key`, `pin`, `pass` and `otp`
//! also appear inside ordinary words, so they need a word boundary before them: the start of
//! the identifier, a non-alphanumeric character or a camelCase step (`apiKey`, `userPin`).
//!
//! The separator is `=`, `:`, `:=`, `=>`, `->`, `==` or the URL-encoded `%3D` and `%3A`, with an
//! optional quote (possibly backslash-escaped) after the label. A quoted value runs to the
//! closing quote and honours backslash escapes. An `Authorization` value is replaced as a whole.
use super::{
    VALUE_STOPS, include_scheme_token, is_marker, is_quote, skip_whitespace, span, value_end,
    word_at,
};

/// Labels that are specific enough to match as the end of any identifier.
const LABELS: [&str; 31] = [
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "credentials",
    "authorizationkey",
    "authkey",
    "apikey",
    "api_key",
    "privatekey",
    "private_key",
    "idtag",
    "id_tag",
    "idtoken",
    "id_token",
    "totp",
    "hotp",
    // Compounds of a short label that have no word boundary when written in one case.
    "secretkey",
    "accesskey",
    "encryptionkey",
    "sessionkey",
    "signingkey",
    "sshkey",
    "wifikey",
    "wpakey",
    "simpin",
    "psk",
    "pswd",
];
/// Short labels that need a word boundary before them.
const SHORT_LABELS: [&str; 4] = ["key", "pin", "pass", "otp"];
/// Labels whose value is a scheme and a credential.
const HEADER_LABELS: [&str; 1] = ["authorization"];

/// Longer forms first, so `:=` is not read as `:`.
const SEPARATORS: [&str; 8] = [":=", "=>", "->", "==", "=", ":", "%3d", "%3a"];

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Value,
    Header,
}

/// An identifier that ends in a label, followed by a separator and a non-empty value.
pub(super) fn labelled_value(chars: &[char], position: usize) -> Option<(usize, usize)> {
    if position > 0 && is_identifier_char(chars[position - 1]) {
        return None;
    }
    let end = identifier_end(chars, position);
    let kind = label_kind(&chars[position..end])?;

    value_after_label(chars, end, kind)
}

fn value_after_label(chars: &[char], label_end: usize, kind: Kind) -> Option<(usize, usize)> {
    let mut cursor = label_end;

    // The label may itself be a quoted key, as in `"password":`.
    let escapes = count_backslashes(chars, cursor);
    if chars.get(cursor + escapes).is_some_and(|c| is_quote(*c)) {
        cursor += escapes + 1;
    }
    cursor = skip_whitespace(chars, cursor);
    cursor = separator_end(chars, cursor)?;
    cursor = skip_whitespace(chars, cursor);

    let escapes = count_backslashes(chars, cursor);
    if let Some(quote) = chars.get(cursor + escapes).filter(|c| is_quote(**c)) {
        let start = cursor + escapes + 1;
        let end = quoted_end(chars, start, *quote, escapes > 0);

        return span(chars, start, end);
    }

    let end = value_end(chars, cursor, VALUE_STOPS);
    if end == cursor || is_marker(&chars[cursor..end]) {
        return None;
    }
    let end = match kind {
        Kind::Value => include_scheme_token(chars, cursor, end),
        Kind::Header => header_value_end(chars, end),
    };

    span(chars, cursor, end)
}

/// The end of a quoted value that starts at `start`. A value opened with `\"` (JSON inside a
/// JSON string) ends at the next `\"` that is followed by the end of the text, whitespace or a
/// delimiter; any other value ends at the first quote that is not backslash-escaped.
fn quoted_end(chars: &[char], start: usize, quote: char, escaped_open: bool) -> usize {
    if escaped_open {
        return (start..chars.len())
            .find(|index| {
                chars[*index] == quote
                    && *index > start
                    && chars[*index - 1] == '\\'
                    && chars.get(*index + 1).is_none_or(|next| closes_value(*next))
            })
            .map_or(chars.len(), |index| index - 1);
    }

    let mut index = start;
    while index < chars.len() {
        if chars[index] == quote {
            return index;
        }
        index += if chars[index] == '\\' { 2 } else { 1 };
    }

    chars.len()
}

fn closes_value(c: char) -> bool {
    c.is_whitespace() || ",;&:}])".contains(c)
}

/// An `Authorization` value is a scheme and its credential; both go.
fn header_value_end(chars: &[char], first_end: usize) -> usize {
    let next = skip_whitespace(chars, first_end);
    if next == first_end {
        return first_end;
    }

    value_end(chars, next, VALUE_STOPS)
}

fn label_kind(identifier: &[char]) -> Option<Kind> {
    let digits = trim_digits(identifier, identifier.len());
    let plural = if digits > 0 && identifier[digits - 1].eq_ignore_ascii_case(&'s') {
        digits - 1
    } else {
        digits
    };

    [
        identifier.len(),
        digits,
        plural,
        trim_digits(identifier, plural),
    ]
    .into_iter()
    .find_map(|end| kind_of_ending(&identifier[..end]))
}

fn kind_of_ending(head: &[char]) -> Option<Kind> {
    if HEADER_LABELS.iter().any(|label| ends_with(head, label)) {
        return Some(Kind::Header);
    }
    let long = LABELS.iter().any(|label| ends_with(head, label));
    let short = SHORT_LABELS
        .iter()
        .any(|label| ends_with(head, label) && boundary_before(head, label.len()));

    (long || short).then_some(Kind::Value)
}

/// A short label that fills the last `label_len` characters of `head` starts a word when the
/// character before it is not alphanumeric, or when it starts a camelCase word (`apiKey`,
/// `SSHKey`).
fn boundary_before(head: &[char], label_len: usize) -> bool {
    let start = head.len() - label_len;
    let Some(before) = start.checked_sub(1).map(|index| head[index]) else {
        return true;
    };
    if !before.is_alphanumeric() {
        return true;
    }

    let first = head[start];
    let rest_is_lower = head[start + 1..].iter().all(|c| c.is_lowercase());

    first.is_uppercase() && (!before.is_uppercase() || rest_is_lower)
}

fn ends_with(head: &[char], label: &str) -> bool {
    let Some(start) = head.len().checked_sub(label.len()) else {
        return false;
    };

    word_at(head, start, label) == Some(head.len())
}

fn trim_digits(chars: &[char], end: usize) -> usize {
    let digits = chars[..end]
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();

    end - digits
}

fn separator_end(chars: &[char], position: usize) -> Option<usize> {
    SEPARATORS
        .iter()
        .find_map(|separator| word_at(chars, position, separator))
}

fn count_backslashes(chars: &[char], position: usize) -> usize {
    chars[position..].iter().take_while(|c| **c == '\\').count()
}

/// The end of the identifier at `start`, without trailing `-` or `.` so that `->` stays a
/// separator.
fn identifier_end(chars: &[char], start: usize) -> usize {
    let length = chars[start..]
        .iter()
        .take_while(|c| is_identifier_char(**c))
        .count();
    let trailing = chars[start..start + length]
        .iter()
        .rev()
        .take_while(|c| matches!(**c, '-' | '.'))
        .count();

    start + length - trailing
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}
