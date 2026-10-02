use super::FRAME_LIMIT;
use serde_json::Value;
use std::collections::BTreeMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn integer(value: &Value, nonnegative: bool) -> bool {
    value
        .as_i64()
        .is_some_and(|n| i32::try_from(n).is_ok() && (!nonnegative || n >= 0))
}

fn timestamp(value: &Value) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value.as_str()?, &Rfc3339).ok()
}

// Parse decimal digits/exponent independently, without floating-point arithmetic.
fn rate(value: &Value) -> bool {
    let Some(number) = value.as_number() else {
        return false;
    };
    let text = number.to_string();
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((&text, "0"));
    let Ok(exponent) = exponent.parse::<i32>() else {
        return false;
    };
    if !(-128..=128).contains(&exponent) || mantissa.starts_with('-') {
        return false;
    }
    let mut digits = String::new();
    let mut fraction = 0i32;
    let mut after_dot = false;
    for byte in mantissa.bytes() {
        if byte == b'.' && !after_dot {
            after_dot = true;
        } else if byte.is_ascii_digit() {
            digits.push(char::from(byte));
            fraction += i32::from(after_dot);
        } else {
            return false;
        }
    }
    let Ok(mut coefficient) = digits.parse::<u128>() else {
        return false;
    };
    if coefficient == 0 {
        return true;
    }
    let power = exponent - fraction + 1;
    if power < 0 {
        for _ in 0..-power {
            if coefficient % 10 != 0 {
                return false;
            }
            coefficient /= 10;
        }
    } else {
        for _ in 0..power {
            let Some(next) = coefficient.checked_mul(10) else {
                return false;
            };
            coefficient = next;
        }
    }
    coefficient <= i64::MAX as u128
}

pub(super) fn structure(payload: &Value) -> bool {
    if !keys(payload, &["evseId", "chargingProfile"])
        || !integer(&payload["evseId"], true)
        || serde_json::to_vec(payload).map_or(true, |bytes| bytes.len() > FRAME_LIMIT)
    {
        return false;
    }
    let p = &payload["chargingProfile"];
    if !keys(
        p,
        &[
            "id",
            "stackLevel",
            "chargingProfilePurpose",
            "chargingProfileKind",
            "recurrencyKind",
            "validFrom",
            "validTo",
            "transactionId",
            "chargingSchedule",
        ],
    ) || !integer(&p["id"], false)
        || !integer(&p["stackLevel"], true)
    {
        return false;
    }
    let purpose = p["chargingProfilePurpose"].as_str().unwrap_or("");
    if ![
        "ChargingStationMaxProfile",
        "ChargingStationExternalConstraints",
        "TxDefaultProfile",
        "TxProfile",
    ]
    .contains(&purpose)
    {
        return false;
    }
    if purpose == "ChargingStationMaxProfile" && payload["evseId"] != 0 {
        return false;
    }
    if purpose == "TxProfile" {
        if payload["evseId"] == 0
            || !p["transactionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.chars().count() <= 36)
        {
            return false;
        }
    } else if p.get("transactionId").is_some() {
        return false;
    }
    let kind = p["chargingProfileKind"].as_str().unwrap_or("");
    if !["Absolute", "Recurring", "Relative"].contains(&kind)
        || (purpose == "ChargingStationMaxProfile" && kind == "Relative")
        || (kind == "Recurring"
            && !matches!(p["recurrencyKind"].as_str(), Some("Daily" | "Weekly")))
        || (kind != "Recurring" && p.get("recurrencyKind").is_some())
    {
        return false;
    }
    if ["validFrom", "validTo"].iter().any(|field| {
        p.get(*field)
            .is_some_and(|value| timestamp(value).is_none())
    }) || matches!((timestamp(&p["validFrom"]), timestamp(&p["validTo"])), (Some(a), Some(b)) if a >= b)
    {
        return false;
    }
    schedule(p, kind)
}

fn schedule(p: &Value, kind: &str) -> bool {
    let Some(schedules) = p["chargingSchedule"].as_array() else {
        return false;
    };
    if schedules.len() != 1 {
        return false;
    }
    let s = &schedules[0];
    if !keys(
        s,
        &[
            "id",
            "startSchedule",
            "duration",
            "chargingRateUnit",
            "minChargingRate",
            "chargingSchedulePeriod",
        ],
    ) || !integer(&s["id"], false)
        || !matches!(s["chargingRateUnit"].as_str(), Some("A" | "W"))
        || s.get("duration").is_some_and(|d| !integer(d, true))
        || s.get("minChargingRate").is_some_and(|n| !rate(n))
        || (kind == "Relative" && s.get("startSchedule").is_some())
        || (kind != "Relative" && timestamp(&s["startSchedule"]).is_none())
    {
        return false;
    }
    let Some(periods) = s["chargingSchedulePeriod"].as_array() else {
        return false;
    };
    if periods.is_empty() || periods.len() > 1024 {
        return false;
    }
    let mut previous = -1;
    for (index, period) in periods.iter().enumerate() {
        if !keys(
            period,
            &["startPeriod", "limit", "numberPhases", "phaseToUse"],
        ) || !integer(&period["startPeriod"], true)
            || !rate(&period["limit"])
            || period
                .get("numberPhases")
                .is_some_and(|n| !matches!(n.as_i64(), Some(1..=3)))
            || period
                .get("phaseToUse")
                .is_some_and(|n| !matches!(n.as_i64(), Some(1..=3)) || period["numberPhases"] != 1)
        {
            return false;
        }
        let start = period["startPeriod"].as_i64().unwrap();
        if (index == 0 && start != 0) || start <= previous {
            return false;
        }
        previous = start;
    }
    true
}

pub(super) fn profile(
    payload: &Value,
    phases: &BTreeMap<u32, bool>,
    transactions: &BTreeMap<u32, String>,
    external: bool,
) -> bool {
    if !structure(payload) {
        return false;
    }
    let p = &payload["chargingProfile"];
    if p["chargingProfilePurpose"] == "ChargingStationExternalConstraints" && !external {
        return false;
    }
    let Some(evse) = payload["evseId"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
    else {
        return false;
    };
    if p["chargingProfilePurpose"] == "TxProfile"
        && transactions.get(&evse).map(String::as_str) != p["transactionId"].as_str()
    {
        return false;
    }
    !p["chargingSchedule"][0]["chargingSchedulePeriod"]
        .as_array()
        .unwrap()
        .iter()
        .any(|period| period.get("phaseToUse").is_some() && phases.get(&evse) != Some(&true))
}

pub(super) fn same_id(a: &Value, b: &Value) -> bool {
    a["chargingProfile"]["id"] == b["chargingProfile"]["id"]
}

pub(super) fn conflicts(a: &Value, b: &Value) -> bool {
    let old = &a["chargingProfile"];
    let new = &b["chargingProfile"];
    if same_id(a, b) {
        return old["chargingProfilePurpose"] == "ChargingStationExternalConstraints";
    }
    if old["chargingProfilePurpose"] != new["chargingProfilePurpose"]
        || old["stackLevel"] != new["stackLevel"]
    {
        return false;
    }
    if new["chargingProfilePurpose"] == "TxProfile" {
        return old["transactionId"] == new["transactionId"];
    }
    let scope = a["evseId"] == b["evseId"]
        || (new["chargingProfilePurpose"] == "TxDefaultProfile"
            && (a["evseId"] == 0 || b["evseId"] == 0));
    scope
        && !matches!((timestamp(&old["validTo"]), timestamp(&new["validFrom"])), (Some(a), Some(b)) if a <= b)
        && !matches!((timestamp(&new["validTo"]), timestamp(&old["validFrom"])), (Some(a), Some(b)) if a <= b)
}

pub(super) fn clear(payload: &Value) -> bool {
    if !keys(payload, &["chargingProfileId", "chargingProfileCriteria"]) {
        return false;
    }
    if let Some(id) = payload.get("chargingProfileId") {
        return payload.get("chargingProfileCriteria").is_none() && integer(id, false);
    }
    let criteria = &payload["chargingProfileCriteria"];
    keys(
        criteria,
        &["evseId", "chargingProfilePurpose", "stackLevel"],
    ) && criteria.as_object().is_some_and(|c| !c.is_empty())
        && ["evseId", "stackLevel"]
            .iter()
            .all(|key| criteria.get(*key).is_none_or(|value| integer(value, true)))
        && criteria.get("chargingProfilePurpose").is_none_or(|p| {
            matches!(
                p.as_str(),
                Some(
                    "ChargingStationMaxProfile"
                        | "ChargingStationExternalConstraints"
                        | "TxDefaultProfile"
                        | "TxProfile"
                )
            )
        })
}

pub(super) fn matches_clear(profile: &Value, payload: &Value) -> bool {
    let p = &profile["chargingProfile"];
    if p["chargingProfilePurpose"] == "ChargingStationExternalConstraints" {
        return false;
    }
    if let Some(id) = payload.get("chargingProfileId") {
        return p["id"] == *id;
    }
    let c = &payload["chargingProfileCriteria"];
    c.get("evseId").is_none_or(|id| profile["evseId"] == *id)
        && c.get("chargingProfilePurpose")
            .is_none_or(|purpose| p["chargingProfilePurpose"] == *purpose)
        && c.get("stackLevel")
            .is_none_or(|stack| p["stackLevel"] == *stack)
}
