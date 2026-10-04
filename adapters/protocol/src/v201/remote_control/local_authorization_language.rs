//! RFC 5646 syntax only. No locale normalization or display behavior.
pub(super) fn valid(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > 8 || !tag.is_ascii() {
        return false;
    }
    if [
        "i-ami",
        "i-bnn",
        "i-hak",
        "i-lux",
        "i-navajo",
        "i-pwn",
        "i-tao",
        "i-tay",
        "i-tsu",
        "no-bok",
        "no-nyn",
        "sgn-BE-FR",
        "sgn-BE-NL",
        "sgn-CH-DE",
        "zh-guoyu",
        "zh-hakka",
        "zh-min",
        "zh-xiang",
    ]
    .iter()
    .any(|grandfathered| tag.eq_ignore_ascii_case(grandfathered))
    {
        return true;
    }
    let mut parts = tag.split('-').peekable();
    let Some(primary) = parts.next() else {
        return false;
    };
    let alnum = |part: &str| {
        !part.is_empty() && part.len() <= 8 && part.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    if primary.eq_ignore_ascii_case("x") {
        return parts.peek().is_some() && parts.all(alnum);
    }
    if !(2..=8).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    if primary.len() <= 3 {
        for _ in 0..3 {
            if parts
                .peek()
                .is_some_and(|p| p.len() == 3 && p.bytes().all(|b| b.is_ascii_alphabetic()))
            {
                parts.next();
            } else {
                break;
            }
        }
    }
    if parts
        .peek()
        .is_some_and(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        parts.next();
    }
    if parts.peek().is_some_and(|p| {
        (p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic()))
            || (p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit()))
    }) {
        parts.next();
    }
    while let Some(part) = parts.peek() {
        if ((5..=8).contains(&part.len())
            || (part.len() == 4 && part.as_bytes()[0].is_ascii_digit()))
            && alnum(part)
        {
            parts.next();
        } else {
            break;
        }
    }
    while let Some(singleton) = parts.next() {
        if singleton.eq_ignore_ascii_case("x") {
            return parts.peek().is_some() && parts.all(alnum);
        }
        if singleton.len() != 1 || !alnum(singleton) {
            return false;
        }
        let mut count = 0;
        while parts.peek().is_some_and(|p| p.len() >= 2 && alnum(p)) {
            parts.next();
            count += 1;
        }
        if count == 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_native_short_regular_grandfathered_and_private_language_tags() {
        for tag in [
            "en", "pl", "en-US", "zh-Hant", "es-419", "de-1996", "i-navajo", "x-a", "en-x-a",
        ] {
            assert!(super::valid(tag), "{tag}");
        }
        for tag in [
            "", "e", "en_Us", "en-", "-en", "en--US", "en-x", "123", "i-fake",
        ] {
            assert!(!super::valid(tag), "{tag}");
        }
    }
}
