use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_contracts::{
    AccessMode, BridgeId, CanonicalConnectorId, CanonicalResource, DataPointConstraints,
    DataPointDescriptor, DataPointValue, EngineeringUnit, ExactDecimal, Freshness,
    MeasurementContext, MeasurementLocation, MeasurementMetadata, MeasurementPhase, NamedEnumValue,
    NativeProtocolReference, PointId, Quality, QualityLevel, ResourceRef, SemanticName, StationId,
    TypedValue, UnitConversionError, UtcTimestamp, ValueType,
};

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture text")
}

fn timestamp(hour: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).expect("fixture date"),
            Time::from_hms(hour, 15, 0).expect("fixture time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

fn point_id() -> PointId {
    text(PointId::new, "station-7.evse-1.power.active")
}

fn resource() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-berlin-1"),
        station_id: text(StationId::new, "station-7"),
        resource: Some(CanonicalResource::Evse {
            evse_id: text(uob_contracts::CanonicalEvseId::new, "evse-1"),
            connector_id: Some(text(CanonicalConnectorId::new, "connector-2")),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: Some(2),
        }),
    }
}

fn measurement_fixture() -> DataPointValue {
    DataPointValue {
        point_id: point_id(),
        value: Some(TypedValue::Decimal(
            ExactDecimal::from_str("12.345678901234567890").expect("exact fixture"),
        )),
        source_time: None,
        observed_at: timestamp(12),
        quality: Quality {
            level: QualityLevel::Uncertain,
            reason: Some("device_clock_unavailable".to_owned()),
        },
        freshness: Freshness::Fresh {
            valid_until: Some(timestamp(13)),
        },
        measurement: Some(MeasurementMetadata {
            original_value: "12345.678901234567890".to_owned(),
            original_unit: Some("W".to_owned()),
            measurand: Some(text(SemanticName::new, "power.active.import")),
            phase: Some(text(MeasurementPhase::new, "L1-N")),
            context: Some(text(MeasurementContext::new, "sample.periodic")),
            location: Some(text(MeasurementLocation::new, "outlet")),
            protocol_reference: Some(NativeProtocolReference::Ocpp201 {
                evse_id: 1,
                connector_id: Some(2),
            }),
        }),
    }
}

#[test]
fn checked_in_fixture_round_trips_without_losing_measurement_semantics() {
    let fixture = include_str!("fixtures/data-point-value-v1.json");
    let decoded: DataPointValue = serde_json::from_str(fixture).expect("decode fixture");

    assert_eq!(decoded, measurement_fixture());
    assert!(decoded.source_time.is_none());
    assert_eq!(
        serde_json::to_value(decoded).expect("re-encode fixture"),
        serde_json::from_str::<serde_json::Value>(fixture).expect("JSON fixture")
    );
}

#[test]
fn exact_decimals_and_integer_boundaries_round_trip() {
    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Boundaries {
        minimum: TypedValue,
        maximum: TypedValue,
        unsigned_maximum: TypedValue,
        tiny_decimal: TypedValue,
    }

    let boundaries = Boundaries {
        minimum: TypedValue::SignedInteger(i64::MIN),
        maximum: TypedValue::SignedInteger(i64::MAX),
        unsigned_maximum: TypedValue::UnsignedInteger(u64::MAX),
        tiny_decimal: TypedValue::Decimal(
            ExactDecimal::from_str("-0.000000000000000001").expect("tiny exact decimal"),
        ),
    };
    let json = serde_json::to_string(&boundaries).expect("encode boundaries");

    assert!(json.contains("\"-0.000000000000000001\""));
    assert_eq!(
        serde_json::from_str::<Boundaries>(&json).expect("decode boundaries"),
        boundaries
    );
}

#[test]
fn supported_unit_conversions_are_exact_and_reversible() {
    let watts = ExactDecimal::from_str("12345.678901234567890").expect("watts");
    let kilowatts = EngineeringUnit::Watt
        .convert(watts, EngineeringUnit::Kilowatt)
        .expect("supported conversion");

    assert_eq!(kilowatts.to_string(), "12.34567890123456789");
    assert_eq!(
        EngineeringUnit::Kilowatt
            .convert(kilowatts, EngineeringUnit::Watt)
            .expect("reverse conversion"),
        watts
    );
    assert_eq!(
        EngineeringUnit::Milliampere
            .convert(
                ExactDecimal::from_str("1250").expect("milliamperes"),
                EngineeringUnit::Ampere,
            )
            .expect("current conversion")
            .to_string(),
        "1.25"
    );
}

#[test]
fn unsupported_unit_conversion_returns_an_explicit_error() {
    let result = EngineeringUnit::Watt.convert(
        ExactDecimal::from_str("1").expect("quantity"),
        EngineeringUnit::Volt,
    );

    assert_eq!(
        result,
        Err(UnitConversionError::IncompatibleUnits {
            source: EngineeringUnit::Watt,
            target: EngineeringUnit::Volt,
        })
    );
}

#[test]
fn unavailable_and_bad_measurements_do_not_invent_zero_or_false() {
    let value = DataPointValue {
        point_id: point_id(),
        value: None,
        source_time: None,
        observed_at: timestamp(12),
        quality: Quality {
            level: QualityLevel::Bad,
            reason: Some("sensor_invalid".to_owned()),
        },
        freshness: Freshness::Unknown,
        measurement: None,
    };
    let json = serde_json::to_value(&value).expect("serialize unavailable value");

    assert!(json.get("value").is_none());
    assert_ne!(value.value, Some(TypedValue::SignedInteger(0)));
    assert_ne!(value.value, Some(TypedValue::Boolean(false)));
}

#[test]
fn descriptors_declare_typed_ranges_and_enum_members() {
    let descriptor = DataPointDescriptor {
        point_id: point_id(),
        resource: resource(),
        semantic_name: text(SemanticName::new, "power.active.import"),
        value_type: ValueType::Decimal,
        unit: Some(EngineeringUnit::Kilowatt),
        access: AccessMode::ReadOnly,
        constraints: DataPointConstraints {
            minimum: Some(TypedValue::Decimal(ExactDecimal::new(0, 0))),
            maximum: Some(TypedValue::Decimal(ExactDecimal::new(22, 0))),
            enum_values: Vec::new(),
        },
    };
    let enum_members = DataPointConstraints {
        minimum: None,
        maximum: None,
        enum_values: vec![
            text(NamedEnumValue::new, "available"),
            text(NamedEnumValue::new, "occupied"),
        ],
    };

    assert_eq!(descriptor.value_type, ValueType::Decimal);
    assert_eq!(
        descriptor
            .constraints
            .maximum
            .as_ref()
            .expect("maximum")
            .value_type(),
        descriptor.value_type
    );
    assert_eq!(enum_members.enum_values.len(), 2);
}

#[test]
fn malformed_or_numeric_json_decimals_are_rejected() {
    assert!(ExactDecimal::from_str("1e3").is_err());
    assert!(ExactDecimal::from_str("NaN").is_err());
    assert!(ExactDecimal::from_str("-+1").is_err());
    assert!(serde_json::from_str::<ExactDecimal>("1.5").is_err());
}
