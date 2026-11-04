//! HEM's per-model capabilities against GivTCP's (`tests/fixtures/
//! givtcp-model-contract.json`).
//!
//! GivTCP has far more users testing it on real hardware, so where HEM gives
//! a model a different pause or register layout the contract must say so,
//! with the issue that will remove the difference. A change to either side
//! that isn't recorded there fails here, and so does a recorded difference
//! that has quietly gone away, so the contract can't go stale either.

use serde::Deserialize;
use std::collections::BTreeMap;

use super::charge_cap;
use super::model::{DeviceType, PauseRegisterSupport};

#[derive(Debug, Deserialize)]
struct Contract {
    rows: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct Row {
    code: String,
    arm_fw: u16,
    hem: String,
    givtcp: Facts,
    #[serde(default)]
    hem_deviations: BTreeMap<String, Deviation>,
}

#[derive(Debug, Deserialize)]
struct Facts {
    pause: String,
    three_phase: bool,
}

#[derive(Debug, Deserialize)]
struct Deviation {
    value: serde_json::Value,
    issue: u32,
    why: String,
}

fn contract() -> Contract {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/givtcp-model-contract.json"
    ))
    .expect("the contract is valid JSON")
}

fn device(row: &Row) -> DeviceType {
    let dtc = u16::from_str_radix(&row.code, 16).expect("hex code");
    DeviceType::detection_trace(dtc, row.arm_fw).refined
}

fn pause_name(support: PauseRegisterSupport) -> &'static str {
    match support {
        PauseRegisterSupport::Unsupported => "none",
        PauseRegisterSupport::ModeOnly => "mode",
        PauseRegisterSupport::ModeAndWindow => "mode_and_window",
    }
}

/// What HEM does for this row, field by field.
fn hem_facts(row: &Row) -> BTreeMap<&'static str, serde_json::Value> {
    let dt = device(row);
    BTreeMap::from([
        (
            "pause",
            serde_json::json!(pause_name(dt.pause_register_support(row.arm_fw))),
        ),
        (
            "three_phase",
            serde_json::json!(dt.uses_three_phase_schedule_slots()),
        ),
        (
            "charge_cap",
            serde_json::json!(charge_cap::supports(dt, row.arm_fw)),
        ),
    ])
}

/// What GivTCP's facts say HEM should do. The charge cap pauses charging,
/// so it belongs wherever GivTCP pauses.
fn expected_facts(row: &Row) -> BTreeMap<&'static str, serde_json::Value> {
    BTreeMap::from([
        ("pause", serde_json::json!(row.givtcp.pause)),
        ("three_phase", serde_json::json!(row.givtcp.three_phase)),
        ("charge_cap", serde_json::json!(row.givtcp.pause != "none")),
    ])
}

#[test]
fn every_row_names_the_model_hem_detects() {
    let contract = contract();
    assert!(contract.rows.len() >= 19, "the contract lost rows");
    for row in &contract.rows {
        assert_eq!(
            format!("{:?}", device(row)),
            row.hem,
            "0x{} on ARM {}",
            row.code,
            row.arm_fw
        );
    }
}

#[test]
fn hem_matches_givtcp_except_where_the_contract_says_why() {
    let mut problems = Vec::new();
    for row in contract().rows {
        let hem = hem_facts(&row);
        let expected = expected_facts(&row);
        for (field, wanted) in &expected {
            let actual = &hem[field];
            let label = format!("0x{} ARM {} ({}) {field}", row.code, row.arm_fw, row.hem);
            match row.hem_deviations.get(*field) {
                None if actual != wanted => problems.push(format!(
                    "{label}: HEM does {actual}, GivTCP {wanted}. Align HEM, or record it under hem_deviations with an issue"
                )),
                Some(deviation) if deviation.value == *wanted => problems.push(format!(
                    "{label}: the recorded deviation is what GivTCP does; drop it"
                )),
                Some(deviation) if *actual != deviation.value => problems.push(format!(
                    "{label}: HEM does {actual}, not the recorded {} (#{}). If #{} is fixed, drop the deviation",
                    deviation.value, deviation.issue, deviation.issue
                )),
                _ => {}
            }
        }
        for field in row.hem_deviations.keys() {
            if !expected.contains_key(field.as_str()) {
                problems.push(format!(
                    "0x{} ARM {}: unknown field {field}",
                    row.code, row.arm_fw
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn every_deviation_says_why_and_which_issue() {
    for row in contract().rows {
        for (field, deviation) in &row.hem_deviations {
            assert!(
                deviation.issue > 0 && !deviation.why.trim().is_empty(),
                "0x{} {field}",
                row.code
            );
        }
    }
}
