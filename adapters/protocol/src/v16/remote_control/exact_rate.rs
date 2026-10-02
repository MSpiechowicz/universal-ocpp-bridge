//! Lossless nonnegative native A/W numeric rates shared by schedule boundaries.
use serde_json::Value;
use std::borrow::Cow;
use uob_contracts::ExactDecimal;
// Original JSON numeric lexemes are checked before accepting the native Decimal's mantissa.
// Decimal::from_str may round at its precision boundary. Never normalize such loss into evidence.
pub(crate) fn exact_rate(value: &Value) -> Option<ExactDecimal> {
    let Value::Number(number) = value else {
        return None;
    };
    let lexeme = number.as_str();
    let unsigned = lexeme.strip_prefix('-').unwrap_or(lexeme);
    let (significand, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((significand, exponent)) => (significand, exponent.parse::<i64>().ok()?),
        None => (unsigned, 0),
    };
    let fraction = significand
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let trailing = significand
        .bytes()
        .rev()
        .filter(|byte| *byte != b'.')
        .take_while(|byte| *byte == b'0')
        .count();
    let digits = significand.bytes().filter(|byte| *byte != b'.');
    let digit_count = significand.len() - usize::from(significand.contains('.'));
    let mut coefficient = 0_i128;
    for digit in digits.take(digit_count - trailing) {
        coefficient = coefficient
            .checked_mul(10)?
            .checked_add(i128::from(digit - b'0'))?;
    }
    if coefficient == 0 {
        return Some(ExactDecimal::new(0, 0));
    }
    if lexeme.starts_with('-') {
        return None;
    }
    let scale = i64::try_from(fraction)
        .ok()?
        .checked_sub(i64::try_from(trailing).ok()?)?
        .checked_sub(exponent)?;
    if scale > 1 {
        return None;
    }
    if scale < 0 {
        let power = u32::try_from(scale.checked_neg()?).ok()?;
        coefficient = coefficient.checked_mul(10_i128.checked_pow(power)?)?;
    }
    // The native Decimal mantissa is unsigned 96-bit. Do not let its decoder round
    // a just-out-of-range tenth into a representable integer.
    if coefficient > (1_i128 << 96) - 1 {
        return None;
    }
    Some(ExactDecimal::new(
        coefficient,
        u32::try_from(scale.max(0)).ok()?,
    ))
}
pub(crate) fn decoder_rate(value: &Value) -> Option<Cow<'_, Value>> {
    let number = value.as_number()?;
    let scientific = number.as_str().contains(['e', 'E']);
    let large_integer = number
        .as_str()
        .parse::<u128>()
        .is_ok_and(|integer| integer > u128::from(u64::MAX));
    if !scientific && !large_integer {
        return Some(Cow::Borrowed(value));
    }

    let rate = exact_rate(value)?;
    let mut lexeme = rate.to_string();
    // The pinned Decimal Value visitor has no u128 support. A decimal point
    // selects its lossless numeric-string/map branch, including normalized exponents.
    if rate.scale() == 0 && rate.coefficient() > i128::from(u64::MAX) {
        lexeme.push_str(".0");
    }
    Some(Cow::Owned(Value::Number(lexeme.parse().ok()?)))
}
