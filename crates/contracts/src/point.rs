use std::{borrow::Cow, error::Error, fmt, str::FromStr};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{NativeProtocolReference, ResourceRef, UtcTimestamp};

macro_rules! point_text {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a value after rejecting empty or whitespace-only text.
            ///
            /// # Errors
            ///
            /// Returns [`PointIdentityError`] when the value is empty.
            pub fn new(value: impl Into<String>) -> Result<Self, PointIdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(PointIdentityError(stringify!($name)));
                }
                Ok(Self(value))
            }

            /// Returns the stable wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

point_text!(PointId, "Stable identity of a canonical data point.");
point_text!(SemanticName, "Versioned semantic name of a data point.");
point_text!(NamedEnumValue, "Named member of a declared enumeration.");
point_text!(MeasurementPhase, "Original measurement phase designation.");
point_text!(MeasurementContext, "Original measurement sampling context.");
point_text!(
    MeasurementLocation,
    "Original measurement location designation."
);

/// Failure to construct a stable point identifier or semantic label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointIdentityError(&'static str);

impl fmt::Display for PointIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be empty", self.0)
    }
}

impl Error for PointIdentityError {}

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

/// Canonical primitive kind declared by a data point.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// Boolean state.
    Boolean,
    /// Signed 64-bit integer.
    SignedInteger,
    /// Unsigned 64-bit integer.
    UnsignedInteger,
    /// Exact base-ten quantity.
    Decimal,
    /// Unicode text.
    Text,
    /// Member of a declared named enumeration.
    NamedEnum,
}

/// Typed canonical value; unavailable measurements are represented by absence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TypedValue {
    /// Boolean state.
    Boolean(bool),
    /// Signed integer preserving all `i64` boundary values.
    SignedInteger(i64),
    /// Unsigned integer preserving all `u64` boundary values.
    UnsignedInteger(u64),
    /// Exact decimal serialized as a string.
    Decimal(ExactDecimal),
    /// Unicode text value.
    Text(String),
    /// Named member of a descriptor-declared enumeration.
    NamedEnum(NamedEnumValue),
}

impl TypedValue {
    /// Returns the descriptor kind corresponding to this value.
    #[must_use]
    pub const fn value_type(&self) -> ValueType {
        match self {
            Self::Boolean(_) => ValueType::Boolean,
            Self::SignedInteger(_) => ValueType::SignedInteger,
            Self::UnsignedInteger(_) => ValueType::UnsignedInteger,
            Self::Decimal(_) => ValueType::Decimal,
            Self::Text(_) => ValueType::Text,
            Self::NamedEnum(_) => ValueType::NamedEnum,
        }
    }
}

/// Read/write behavior exposed for a canonical point.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// Point can only be observed.
    ReadOnly,
    /// Point can only be written through an explicit authorized operation.
    WriteOnly,
    /// Point can be observed and written through explicit authorized operations.
    ReadWrite,
}

/// Engineering unit with an explicitly supported normalization policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringUnit {
    /// Ampere.
    Ampere,
    /// Milliampere.
    Milliampere,
    /// Volt.
    Volt,
    /// Millivolt.
    Millivolt,
    /// Watt.
    Watt,
    /// Kilowatt.
    Kilowatt,
    /// Watt-hour.
    WattHour,
    /// Kilowatt-hour.
    KilowattHour,
    /// Volt-ampere.
    VoltAmpere,
    /// Kilovolt-ampere.
    KilovoltAmpere,
    /// Volt-ampere VAR.
    Var,
    /// Kilovolt-ampere VAR.
    Kilovar,
    /// Hertz.
    Hertz,
    /// Degrees Celsius.
    Celsius,
    /// Percentage.
    Percent,
}

impl EngineeringUnit {
    /// Converts a value between supported units of the same physical dimension.
    ///
    /// # Errors
    ///
    /// Returns [`UnitConversionError::IncompatibleUnits`] for different physical
    /// dimensions and [`UnitConversionError::Overflow`] when the exact result is
    /// outside [`ExactDecimal`] bounds.
    pub fn convert(
        self,
        value: ExactDecimal,
        target: Self,
    ) -> Result<ExactDecimal, UnitConversionError> {
        let (source_dimension, source_exponent) = self.dimension_and_exponent();
        let (target_dimension, target_exponent) = target.dimension_and_exponent();
        if source_dimension != target_dimension {
            return Err(UnitConversionError::IncompatibleUnits {
                source: self,
                target,
            });
        }
        value
            .checked_scale_by_power_of_ten(source_exponent - target_exponent)
            .map_err(|_| UnitConversionError::Overflow)
    }

    const fn dimension_and_exponent(self) -> (Dimension, i32) {
        match self {
            Self::Ampere => (Dimension::Current, 0),
            Self::Milliampere => (Dimension::Current, -3),
            Self::Volt => (Dimension::Voltage, 0),
            Self::Millivolt => (Dimension::Voltage, -3),
            Self::Watt => (Dimension::Power, 0),
            Self::Kilowatt => (Dimension::Power, 3),
            Self::WattHour => (Dimension::Energy, 0),
            Self::KilowattHour => (Dimension::Energy, 3),
            Self::VoltAmpere => (Dimension::ApparentPower, 0),
            Self::KilovoltAmpere => (Dimension::ApparentPower, 3),
            Self::Var => (Dimension::VarPower, 0),
            Self::Kilovar => (Dimension::VarPower, 3),
            Self::Hertz => (Dimension::Frequency, 0),
            Self::Celsius => (Dimension::Temperature, 0),
            Self::Percent => (Dimension::Ratio, 0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dimension {
    Current,
    Voltage,
    Power,
    Energy,
    ApparentPower,
    VarPower,
    Frequency,
    Temperature,
    Ratio,
}

/// Explicit failure to normalize an engineering quantity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnitConversionError {
    /// The units describe different physical dimensions.
    IncompatibleUnits {
        /// Unit supplied by the source.
        source: EngineeringUnit,
        /// Requested normalized unit.
        target: EngineeringUnit,
    },
    /// The exact result is outside the decimal representation bounds.
    Overflow,
}

impl fmt::Display for UnitConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleUnits { source, target } => {
                write!(formatter, "cannot convert {source:?} to {target:?}")
            }
            Self::Overflow => formatter.write_str("unit conversion exceeds decimal bounds"),
        }
    }
}

impl Error for UnitConversionError {}

/// Optional declared bounds and members for a point.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DataPointConstraints {
    /// Inclusive lower numeric bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<TypedValue>,
    /// Inclusive upper numeric bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<TypedValue>,
    /// Allowed members for a named enumeration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<NamedEnumValue>,
}

/// Stable description of one canonical data point.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DataPointDescriptor {
    /// Stable point identity.
    pub point_id: PointId,
    /// Canonical charging resource that owns the point.
    pub resource: ResourceRef,
    /// Versioned semantic meaning, independent of target naming.
    pub semantic_name: SemanticName,
    /// Primitive type accepted by the point.
    pub value_type: ValueType,
    /// Canonical engineering unit, when the value is a quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<EngineeringUnit>,
    /// Declared observation/control access.
    pub access: AccessMode,
    /// Optional range or enumeration declaration.
    #[serde(default, skip_serializing_if = "DataPointConstraints::is_empty")]
    pub constraints: DataPointConstraints,
}

impl DataPointConstraints {
    pub(crate) fn is_empty(&self) -> bool {
        self.minimum.is_none() && self.maximum.is_none() && self.enum_values.is_empty()
    }
}

/// Data quality classification independent of freshness.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityLevel {
    /// Source and bridge consider the value valid.
    Good,
    /// Value may be usable with a stated qualification.
    Uncertain,
    /// Value must not be used as a valid observation.
    Bad,
}

/// Quality classification and optional machine-readable reason.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct Quality {
    /// Coarse quality level.
    pub level: QualityLevel,
    /// Stable reason code supplied when more detail is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Freshness state tracked independently from data quality.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Freshness {
    /// Value is within its declared freshness interval.
    Fresh {
        /// UTC instant after which the value becomes stale, when configured.
        #[serde(skip_serializing_if = "Option::is_none")]
        valid_until: Option<UtcTimestamp>,
    },
    /// Value is older than the accepted freshness interval.
    Stale,
    /// No freshness policy or evidence is available.
    Unknown,
}

/// Protocol semantics retained alongside a normalized measurement.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MeasurementMetadata {
    /// Exact original textual value before normalization.
    pub original_value: String,
    /// Original unit token, including tokens not supported for conversion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_unit: Option<String>,
    /// Original measurand name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurand: Option<SemanticName>,
    /// Original phase designation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<MeasurementPhase>,
    /// Original sampling context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<MeasurementContext>,
    /// Original measurement location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<MeasurementLocation>,
    /// Original OCPP resource address, independent of canonical identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_reference: Option<NativeProtocolReference>,
}

/// One observed point value with timestamps, quality, and freshness kept together.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DataPointValue {
    /// Stable identity of the described point.
    pub point_id: PointId,
    /// Typed value; `None` means explicitly unavailable, never numeric zero or false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<TypedValue>,
    /// Device/source timestamp, absent when none was provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<UtcTimestamp>,
    /// UTC instant at which the bridge observed the value or its unavailability.
    pub observed_at: UtcTimestamp,
    /// Quality and its reason, independent of age.
    pub quality: Quality,
    /// Freshness metadata, independent of quality.
    pub freshness: Freshness,
    /// Measurement-specific original semantics, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<MeasurementMetadata>,
}
