//! Validate syntax/depth/numbers before allocating decoded sensitive strings.
/// A borrowed `RawValue` validates strings and JSON syntax without owning decoded secrets.
/// The subsequent `Value` parse cannot fail midway on excessive depth or number range.
pub(super) fn safe_to_decode(bytes: &[u8]) -> bool {
    if serde_json::from_slice::<&serde_json::value::RawValue>(bytes).is_err() {
        return false;
    }
    let mut index = 0;
    let mut depth = 0usize;
    let mut quoted = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 16 {
                        return false;
                    }
                }
                b'}' | b']' => depth -= 1,
                b'-' | b'0'..=b'9' => {
                    let start = index;
                    while index < bytes.len()
                        && matches!(bytes[index], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                    {
                        index += 1;
                    }
                    if serde_json::from_slice::<serde_json::Number>(&bytes[start..index]).is_err() {
                        return false;
                    }
                    continue;
                }
                _ => {}
            }
        }
        index += 1;
    }
    true
}
