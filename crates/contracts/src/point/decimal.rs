use std::{borrow::Cow, error::Error, fmt, str::FromStr};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Exact base-ten decimal represented without binary floating point.
///
/// The coefficient and scale representation covers values from `i128::MIN`
/// through `i128::MAX`, with up to `u32::MAX` fractional decimal places. Values
/// serialize as canonical decimal strings so JSON transports retain precision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactDecimal {
    coefficient: i128,
    scale: u32,
}

impl JsonSchema for ExactDecimal {
    fn schema_name() -> Cow<'static, str> {
        "ExactDecimal".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ExactDecimal").into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": r"^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$"
        })
    }
}

impl ExactDecimal {
    /// Creates and normalizes an exact decimal from its coefficient and scale.
    #[must_use]
    pub const fn new(mut coefficient: i128, mut scale: u32) -> Self {
        if coefficient == 0 {
            return Self {
                coefficient: 0,
                scale: 0,
            };
        }
        while scale > 0 && coefficient % 10 == 0 {
            coefficient /= 10;
            scale -= 1;
        }
        Self { coefficient, scale }
    }

    /// Returns the exact signed coefficient.
    #[must_use]
    pub const fn coefficient(self) -> i128 {
        self.coefficient
    }

    /// Returns the number of fractional decimal digits.
    #[must_use]
    pub const fn scale(self) -> u32 {
        self.scale
    }

    /// Applies an exact power-of-ten scale change.
    ///
    /// # Errors
    ///
    /// Returns [`ExactDecimalError::Overflow`] when the adjusted value cannot be
    /// represented by the coefficient/scale bounds.
    pub fn checked_scale_by_power_of_ten(self, exponent: i32) -> Result<Self, ExactDecimalError> {
        if exponent == 0 || self.coefficient == 0 {
            return Ok(self);
        }

        if exponent < 0 {
            let increase = exponent.unsigned_abs();
            let scale = self
                .scale
                .checked_add(increase)
                .ok_or(ExactDecimalError::Overflow)?;
            return Ok(Self::new(self.coefficient, scale));
        }

        let mut exponent = exponent.unsigned_abs();
        let removable = exponent.min(self.scale);
        let scale = self.scale - removable;
        exponent -= removable;
        let factor = checked_power_of_ten(exponent)?;
        let coefficient = self
            .coefficient
            .checked_mul(factor)
            .ok_or(ExactDecimalError::Overflow)?;
        Ok(Self::new(coefficient, scale))
    }
}

fn checked_power_of_ten(exponent: u32) -> Result<i128, ExactDecimalError> {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value.checked_mul(10).ok_or(ExactDecimalError::Overflow)?;
    }
    Ok(value)
}

impl FromStr for ExactDecimal {
    type Err = ExactDecimalError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.trim() != value {
            return Err(ExactDecimalError::InvalidSyntax);
        }

        let (negative, unsigned) = if let Some(unsigned) = value.strip_prefix('-') {
            (true, unsigned)
        } else {
            (false, value.strip_prefix('+').unwrap_or(value))
        };
        let mut parts = unsigned.split('.');
        let integer = parts.next().ok_or(ExactDecimalError::InvalidSyntax)?;
        let fraction = parts.next();
        if parts.next().is_some()
            || integer.is_empty()
            || !integer.bytes().all(|character| character.is_ascii_digit())
            || fraction.is_some_and(|digits| {
                digits.is_empty() || !digits.bytes().all(|character| character.is_ascii_digit())
            })
        {
            return Err(ExactDecimalError::InvalidSyntax);
        }

        let fraction = fraction.unwrap_or_default();
        let scale = u32::try_from(fraction.len()).map_err(|_| ExactDecimalError::Overflow)?;
        let digits = format!("{integer}{fraction}");
        let signed = if negative {
            format!("-{digits}")
        } else {
            digits
        };
        let coefficient = signed
            .parse::<i128>()
            .map_err(|_| ExactDecimalError::Overflow)?;
        Ok(Self::new(coefficient, scale))
    }
}

impl fmt::Display for ExactDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scale == 0 {
            return write!(formatter, "{}", self.coefficient);
        }

        let negative = self.coefficient.is_negative();
        let digits = self.coefficient.unsigned_abs().to_string();
        let scale = usize::try_from(self.scale).map_err(|_| fmt::Error)?;
        if negative {
            formatter.write_str("-")?;
        }
        if digits.len() <= scale {
            formatter.write_str("0.")?;
            for _ in 0..(scale - digits.len()) {
                formatter.write_str("0")?;
            }
            formatter.write_str(&digits)
        } else {
            let split = digits.len() - scale;
            write!(formatter, "{}.{}", &digits[..split], &digits[split..])
        }
    }
}

impl Serialize for ExactDecimal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ExactDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// Failure to parse or manipulate an exact decimal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactDecimalError {
    /// The input was not a plain base-ten decimal string.
    InvalidSyntax,
    /// The coefficient or scale exceeded the supported representation.
    Overflow,
}

impl fmt::Display for ExactDecimalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSyntax => formatter.write_str("invalid exact decimal syntax"),
            Self::Overflow => formatter.write_str("exact decimal exceeds supported bounds"),
        }
    }
}

impl Error for ExactDecimalError {}
