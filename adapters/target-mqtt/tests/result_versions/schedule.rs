use serde_json::{Value, json};
use uob_contracts::{
    CommandLifecycle, CommandResult, CompositeSchedule16, CompositeSchedulePeriod16,
    CompositeScheduleRateUnit16, CompositeScheduleRequest16, CompositeScheduleResult16,
    CompositeScheduleStatus16, ContractVersion, ExactDecimal,
};

use crate::support::fixtures::timestamp;

pub(super) fn attach(result: &mut CommandResult) {
    result.schema_version = ContractVersion::V1_COMPOSITE_SCHEDULE_16;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    result.configuration = None;
    result.composite_schedule_16 = Some(CompositeScheduleResult16 {
        request: CompositeScheduleRequest16 {
            connector_id: 0,
            duration: 120,
            charging_rate_unit: Some(CompositeScheduleRateUnit16::W),
        },
        status: CompositeScheduleStatus16::Accepted,
        connector_id: Some(0),
        schedule_start: Some(timestamp()),
        charging_schedule: Some(CompositeSchedule16 {
            duration: Some(120),
            start_schedule: None,
            charging_rate_unit: CompositeScheduleRateUnit16::W,
            charging_schedule_period: vec![
                CompositeSchedulePeriod16 {
                    start_period: 0,
                    limit: ExactDecimal::new(9_007_199_254_740_991, 1),
                    number_phases: Some(3),
                },
                CompositeSchedulePeriod16 {
                    start_period: 60,
                    limit: ExactDecimal::new(0, 0),
                    number_phases: None,
                },
            ],
            min_charging_rate: Some(ExactDecimal::new(61, 1)),
        }),
    });
}

pub(super) fn assert_payload(payload: &[u8]) {
    let value: Value = serde_json::from_slice(payload).expect("schedule result JSON");
    assert_eq!(value["schema_version"], json!({"major": 1, "revision": 4}));
    assert_eq!(value["lifecycle"]["stage"], "protocol_response");
    assert_eq!(value["lifecycle"]["accepted"], true);
    let schedule = &value["composite_schedule_16"];
    assert_eq!(schedule["status"], "Accepted");
    assert_eq!(schedule["request"]["connector_id"], 0);
    assert_eq!(schedule["request"]["duration"], 120);
    assert_eq!(schedule["request"]["charging_rate_unit"], "W");
    assert_eq!(schedule["connector_id"], 0);
    let charging = &schedule["charging_schedule"];
    assert_eq!(charging["charging_rate_unit"], "W");
    assert_eq!(
        charging["charging_schedule_period"][0]["limit"],
        "900719925474099.1"
    );
    assert_eq!(charging["charging_schedule_period"][1]["start_period"], 60);
    assert_eq!(charging["charging_schedule_period"][1]["limit"], "0");
    assert_eq!(charging["min_charging_rate"], "6.1");
    let decoded: CommandResult = serde_json::from_slice(payload).expect("typed schedule result");
    let schedule = decoded
        .composite_schedule_16
        .expect("native schedule evidence");
    assert_eq!(schedule.status, CompositeScheduleStatus16::Accepted);
    let charging = schedule.charging_schedule.expect("accepted schedule");
    assert_eq!(
        charging.charging_schedule_period[0].limit,
        ExactDecimal::new(9_007_199_254_740_991, 1)
    );
    assert_eq!(
        charging.charging_schedule_period[1].limit,
        ExactDecimal::new(0, 0)
    );
}
