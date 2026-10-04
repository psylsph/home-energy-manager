//! Per-cycle device reads that follow the inverter's own registers: the
//! battery BMS (LV packs at 0x32-0x37, HV stacks via BCU 0x70+ and BMU 0x50+)
//! and the external CT meters.
//!
//! Each read asks a device for a block of input registers, validates it,
//! decodes it into the snapshot, and updates whatever tracking state the poll
//! loop keeps. They are generic over [`RegisterReader`] so they can be tested
//! with a scripted reader instead of a socket, and take the inter-read `pace`
//! as a parameter so tests do not sleep.
//!
//! Two protocols exist (per givenergy-modbus `model/hv_bcu.py` and GivTCP):
//!
//! - **LV packs**: BMS at 0x32 (battery #1) + 0x33-0x37, IR 60-119.
//! - **HV stacks**: BCU at 0x70+i (cluster) + BMU at 0x50+m, IR 60-119. HV
//!   modules do NOT answer at 0x32.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::inverter::decoder::HvBcuCluster;
use crate::inverter::model::{DeviceType, InverterSnapshot, MeterData};
use crate::inverter::poll::{
    hv_bcu_probe_offsets, hv_bcu_probe_should_stop, hv_probe_completed,
    record_external_meter_failure, should_probe_hv_stacks, track_battery_conn,
    valid_lv_battery_response, AppState, HV_MAX_BCU_COUNT,
};
use crate::modbus::client::ModbusClient;
use crate::modbus::framer::RegisterType;

/// Pause between consecutive device reads, so a slow dongle is not hammered.
pub(crate) const DEVICE_READ_PACE: Duration = Duration::from_millis(100);

/// Reads input registers from a specific device address.
#[allow(async_fn_in_trait)]
pub(crate) trait RegisterReader {
    async fn read_input(&mut self, slave: u8, start: u16, count: u16) -> Result<Vec<u16>, String>;
}

impl RegisterReader for ModbusClient {
    async fn read_input(&mut self, slave: u8, start: u16, count: u16) -> Result<Vec<u16>, String> {
        // Read at the given device address without disturbing the
        // model-specific slave the main poll cycle uses.
        self.read_registers_at_slave(slave, RegisterType::Input, start, count)
            .await
            .map_err(|e| e.to_string())
    }
}

// ===========================================================================
// SOC rules
// ===========================================================================

/// When the inverter's own SOC (IR 59) reads 0 - a corrupted read - fall back
/// to battery #1's BMS SOC, but only when that is a believable 1-99%.
pub(crate) fn apply_bms_soc_fallback(snapshot: &mut InverterSnapshot) {
    if snapshot.soc == 0 {
        if let Some(bms) = snapshot.battery_modules.first() {
            if bms.soc > 0 && bms.soc <= 99 {
                snapshot.soc = bms.soc;
            }
        }
    }
}

/// When the inverter's SOC is still 0 once several batteries have been read,
/// use the capacity-weighted average of all modules.
pub(crate) fn apply_aggregate_soc(snapshot: &mut InverterSnapshot) {
    if snapshot.soc == 0 && snapshot.battery_modules.len() > 1 {
        let total_cap: f32 = snapshot.battery_modules.iter().map(|m| m.capacity_ah).sum();
        let total_rem: f32 = snapshot
            .battery_modules
            .iter()
            .map(|m| m.remaining_capacity_ah)
            .sum();
        if total_cap > 0.0 {
            let agg = (total_rem / total_cap * 100.0).round() as u8;
            snapshot.soc = agg.min(100);
            tracing::debug!(
                "Inverter SOC was 0 - aggregate from {} modules: {}%",
                snapshot.battery_modules.len(),
                snapshot.soc
            );
        }
    }
}

// ===========================================================================
// LV batteries
// ===========================================================================

/// Read battery #1's BMS (device 0x32, IR 60-119) and probe additional LV
/// batteries at 0x33-0x37, decoding each into the snapshot and feeding the
/// connection-loss tracking.
///
/// Battery #1's IR 60-119 is NOT part of the standard poll blocks (those only
/// read IR 0-59), so it needs its own read. The model-specific operational read
/// address is not used: AC/Gen1 switch to 0x31 and newer models use 0x11, while
/// the first LV battery BMS stays at 0x32.
pub(crate) async fn read_lv_batteries<R: RegisterReader>(
    reader: &mut R,
    state: &Arc<AppState>,
    snapshot: &mut InverterSnapshot,
    known_battery_addrs: &mut Vec<u8>,
) {
    match reader.read_input(0x32, 60, 60).await {
        Ok(data) => {
            let soc = *data.get(100 - 60).unwrap_or(&0) as u8;
            if valid_lv_battery_response(&data) {
                crate::inverter::decoder::decode_battery_block_into(&data, 0, snapshot, "");
                tracing::debug!("Battery #1 BMS read OK");
                track_battery_conn(state, 1, 0x32, true).await;
                apply_bms_soc_fallback(snapshot);
            } else {
                tracing::debug!("Battery #1 BMS data invalid: SOC={soc}");
                track_battery_conn(state, 1, 0x32, false).await;
            }
        }
        Err(e) => {
            tracing::debug!("Battery #1 BMS read skipped: {e}");
            track_battery_conn(state, 1, 0x32, false).await;
        }
    }

    // Probe additional LV batteries (device addresses 0x33-0x37). The first
    // address that does not answer ends the scan.
    for (i, &addr) in crate::modbus::registers::LV_BATTERY_ADDRESSES
        .iter()
        .enumerate()
    {
        match reader.read_input(addr, 60, 60).await {
            Ok(data) => {
                let soc = *data.get(100 - 60).unwrap_or(&0) as u8;
                if valid_lv_battery_response(&data) {
                    crate::inverter::decoder::decode_battery_block_into(&data, i + 1, snapshot, "");
                    track_battery_conn(state, i + 2, addr, true).await;
                    if !known_battery_addrs.contains(&addr) {
                        tracing::info!(
                            "Battery #{} detected at addr 0x{:02X} (SOC={}%)",
                            i + 2,
                            addr,
                            soc
                        );
                        known_battery_addrs.push(addr);
                    } else {
                        tracing::debug!("Battery #{} at addr 0x{:02X} (SOC={}%)", i + 2, addr, soc);
                    }
                } else {
                    tracing::debug!("Battery addr 0x{:02X}: SOC={} - not present", addr, soc);
                    // A known battery answering with invalid data counts as a
                    // failed read, only for addresses we have seen answer.
                    if known_battery_addrs.contains(&addr) {
                        track_battery_conn(state, i + 2, addr, false).await;
                    }
                    break;
                }
            }
            Err(e) => {
                tracing::debug!("Battery addr 0x{:02X}: no response: {e}", addr);
                if known_battery_addrs.contains(&addr) {
                    track_battery_conn(state, i + 2, addr, false).await;
                }
                break;
            }
        }
    }
}

// ===========================================================================
// HV batteries
// ===========================================================================

/// What the loop has learned about the HV stacks this session.
#[derive(Debug, Default)]
pub(crate) struct HvStacks {
    /// `(bcu_offset, module_count)` for each detected stack.
    pub detected: Vec<(u8, u8)>,
    /// A probe found a usable layout. An empty attempt stays retryable on a slow
    /// cadence so a startup timeout cannot hide the battery for the whole TCP
    /// session.
    pub probe_done: bool,
    pub probe_attempted: bool,
    pub cycles_since_last: u8,
}

/// Discover the BCU layout via the BMS at 0xA0, falling back to a direct probe
/// of BCU 0x70 for single-stack installs that do not expose the aggregation.
async fn probe_hv_stacks<R: RegisterReader>(reader: &mut R, pace: Duration) -> Vec<(u8, u8)> {
    let mut found: Vec<(u8, u8)> = Vec::new();
    // BMS at 0xA0 reports the number of BCUs at IR(61).
    match reader
        .read_input(crate::modbus::registers::HV_BMS_ADDRESS, 60, 5)
        .await
    {
        Ok(bms) => {
            let raw_num_bcus = *bms.get(1).unwrap_or(&0);
            let bcu_offsets = hv_bcu_probe_offsets(raw_num_bcus);
            if raw_num_bcus > HV_MAX_BCU_COUNT {
                tracing::warn!(
                    raw_num_bcus,
                    max_supported = HV_MAX_BCU_COUNT,
                    "BMS reported an invalid HV BCU count; capping probe"
                );
            }
            tracing::info!(
                num_bcus = bcu_offsets.len(),
                "BMS reports {} HV BCU stack(s) after validation",
                bcu_offsets.len()
            );
            let mut consecutive_missing = 0;
            for offset in bcu_offsets {
                // Each BCU's IR(64) holds its module count.
                let bcu_addr = crate::modbus::registers::HV_BCU_BASE_ADDRESS.wrapping_add(offset);
                let present = match reader.read_input(bcu_addr, 60, 60).await {
                    Ok(data) if crate::inverter::decoder::validate_hv_bcu(&data) => {
                        let cluster = crate::inverter::decoder::decode_hv_bcu_cluster(&data);
                        tracing::info!(
                            bcu_offset = offset,
                            modules = cluster.number_of_modules,
                            version = %cluster.pack_software_version,
                            "HV BCU at 0x{bcu_addr:02X} - {} modules",
                            cluster.number_of_modules
                        );
                        found.push((offset, cluster.number_of_modules as u8));
                        true
                    }
                    Ok(_) => {
                        tracing::debug!(
                            bcu_offset = offset,
                            "BCU 0x{bcu_addr:02X} probe: invalid version - no stack"
                        );
                        false
                    }
                    Err(e) => {
                        tracing::debug!(
                            bcu_offset = offset,
                            "BCU 0x{bcu_addr:02X} probe: no response: {e}"
                        );
                        false
                    }
                };
                if present {
                    consecutive_missing = 0;
                } else {
                    consecutive_missing += 1;
                    if hv_bcu_probe_should_stop(consecutive_missing) {
                        tracing::debug!(
                            bcu_offset = offset,
                            consecutive_missing,
                            "Stopping HV BCU probe after consecutive missing stacks"
                        );
                        break;
                    }
                }
                tokio::time::sleep(pace).await;
            }
        }
        Err(e) => {
            tracing::debug!("BMS 0xA0 probe failed: {e} - falling back to direct BCU 0x70 probe");
            if let Ok(data) = reader
                .read_input(crate::modbus::registers::HV_BCU_BASE_ADDRESS, 60, 60)
                .await
            {
                if crate::inverter::decoder::validate_hv_bcu(&data) {
                    let cluster = crate::inverter::decoder::decode_hv_bcu_cluster(&data);
                    found.push((0, cluster.number_of_modules as u8));
                }
            }
        }
    }
    found
}

/// One cycle of HV battery reads: (re)probe for stacks when due, read each
/// stack's BCU cluster, then each BMU's cell data into the snapshot, and
/// backfill the per-module fields the BMU does not expose. Returns the first
/// cluster read, which the caller uses to derive battery fields.
pub(crate) async fn read_hv_battery<R: RegisterReader>(
    reader: &mut R,
    stacks: &mut HvStacks,
    device_type: Option<DeviceType>,
    snapshot: &mut InverterSnapshot,
    pace: Duration,
) -> Option<HvBcuCluster> {
    // Empty attempts stay retryable on a slow cadence, so startup timeouts
    // recover without adding a BMS timeout to every poll.
    if stacks.probe_attempted && !stacks.probe_done {
        stacks.cycles_since_last = stacks.cycles_since_last.saturating_add(1);
    }
    if should_probe_hv_stacks(
        device_type,
        stacks.probe_done,
        stacks.probe_attempted,
        stacks.cycles_since_last,
    ) {
        stacks.probe_attempted = true;
        stacks.cycles_since_last = 0;
        tracing::info!("Probing for HV battery BCU stacks...");
        stacks.detected = probe_hv_stacks(reader, pace).await;
        stacks.probe_done = hv_probe_completed(&stacks.detected);
        if stacks.detected.is_empty() {
            tracing::info!("No HV battery BCU stacks detected");
        } else {
            tracing::info!(
                "Detected {} HV BCU stack(s): {:?}",
                stacks.detected.len(),
                stacks.detected
            );
        }
    }

    // Read each detected stack's cluster block this cycle.
    let mut hv_cluster: Option<HvBcuCluster> = None;
    for &(offset, _modules) in &stacks.detected {
        let bcu_addr = crate::modbus::registers::HV_BCU_BASE_ADDRESS.wrapping_add(offset);
        match reader.read_input(bcu_addr, 60, 60).await {
            Ok(data) if crate::inverter::decoder::validate_hv_bcu(&data) => {
                let cluster = crate::inverter::decoder::decode_hv_bcu_cluster(&data);
                tracing::debug!(
                    bcu_offset = offset,
                    voltage = cluster.battery_voltage,
                    current = cluster.battery_current,
                    modules = cluster.number_of_modules,
                    "HV BCU cluster read OK"
                );
                if hv_cluster.is_none() {
                    hv_cluster = Some(cluster);
                }
            }
            Ok(_) => {
                tracing::debug!(
                    bcu_offset = offset,
                    "HV BCU 0x{bcu_addr:02X} read: invalid version"
                );
            }
            Err(e) => {
                tracing::debug!(
                    bcu_offset = offset,
                    "HV BCU 0x{bcu_addr:02X} read failed: {e}"
                );
            }
        }
    }

    // Each BMU (device 0x50+m) exposes one module's cell-level data for the
    // Battery page. The read base shifts by 120*bcu_offset so the returned
    // slice always starts at v_cell_01 (per GivTCP's read convention;
    // givenergy-modbus resolves the same layout via the BMU stride within a BCU).
    let mut module_index: usize = 0;
    for &(offset, num_modules) in &stacks.detected {
        let base = 60u16 + 120u16 * offset as u16;
        for bmu_num in 0..num_modules {
            let bmu_addr = crate::modbus::registers::HV_BMU_BASE_ADDRESS.wrapping_add(bmu_num);
            match reader.read_input(bmu_addr, base, 60).await {
                Ok(data) if crate::inverter::decoder::validate_hv_bmu(&data) => {
                    let module = crate::inverter::decoder::decode_hv_bmu_block(&data, module_index);
                    tracing::debug!(
                        bcu_offset = offset,
                        bmu = bmu_num,
                        module = module_index,
                        cells = module.cell_voltages.len(),
                        voltage = module.voltage,
                        "HV BMU read OK"
                    );
                    snapshot.battery_modules.push(module);
                }
                Ok(_) => {
                    tracing::debug!(
                        bcu_offset = offset,
                        bmu = bmu_num,
                        "HV BMU 0x{bmu_addr:02X}: invalid serial - not present"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        bcu_offset = offset,
                        bmu = bmu_num,
                        "HV BMU 0x{bmu_addr:02X}: no response: {e}"
                    );
                }
            }
            // The index counts positions, so an absent module leaves a gap.
            module_index += 1;
            tokio::time::sleep(pace).await;
        }
    }

    // HV BMU modules do not expose a per-module SOC register (confirmed against
    // GivTCP's hvbmu.py - the BMU bank is cell voltages, cell temps and serial
    // only). The BCU cluster reports the stack-wide SOC spread and per-module Ah
    // capacity, which we backfill onto each module so the Battery page shows a
    // sensible non-zero per-module SOC and capacity instead of 0%.
    if let Some(cluster) = &hv_cluster {
        crate::inverter::decoder::backfill_hv_module_fields(&mut snapshot.battery_modules, cluster);
    }
    hv_cluster
}

// ===========================================================================
// External CT meters
// ===========================================================================

/// Read every previously detected meter. A meter that stops responding is
/// tolerated for a few cycles (its cached reading is kept) before it is
/// dropped; see [`record_external_meter_failure`].
pub(crate) async fn read_external_meters<R: RegisterReader>(
    reader: &mut R,
    detected_meters: &[u8],
    cached: &mut BTreeMap<u8, MeterData>,
    failures: &mut BTreeMap<u8, u8>,
    pace: Duration,
) {
    for &addr in detected_meters {
        match reader.read_input(addr, 60, 30).await {
            Ok(data) => {
                let meter = crate::inverter::decoder::decode_meter_data(&data, addr);
                cached.insert(addr, meter);
                failures.remove(&addr);
            }
            Err(e) => {
                record_external_meter_failure(failures, cached, addr);
                tracing::debug!("Meter addr 0x{addr:02X}: read failed: {e}");
            }
        }
        tokio::time::sleep(pace).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::BATTERY_CONNECTION_LOST_CONFIRM_CYCLES;
    use crate::inverter::model::BatteryModule;
    use crate::test_util::with_isolated_config_dir_async;
    use std::collections::HashMap;

    const ZERO: Duration = Duration::ZERO;

    /// Answers reads from a script and records every request.
    #[derive(Default)]
    struct Scripted {
        responses: HashMap<(u8, u16), Result<Vec<u16>, String>>,
        calls: Vec<(u8, u16, u16)>,
    }

    impl Scripted {
        fn ok(&mut self, slave: u8, start: u16, data: Vec<u16>) -> &mut Self {
            self.responses.insert((slave, start), Ok(data));
            self
        }
        fn fail(&mut self, slave: u8, start: u16) -> &mut Self {
            self.responses
                .insert((slave, start), Err("no response".into()));
            self
        }
        fn asked(&self, slave: u8) -> usize {
            self.calls.iter().filter(|c| c.0 == slave).count()
        }
    }

    impl RegisterReader for Scripted {
        async fn read_input(
            &mut self,
            slave: u8,
            start: u16,
            count: u16,
        ) -> Result<Vec<u16>, String> {
            self.calls.push((slave, start, count));
            self.responses
                .get(&(slave, start))
                .cloned()
                .unwrap_or_else(|| Err("no response".into()))
        }
    }

    // ---- block builders --------------------------------------------------------

    /// A present LV BMS block (IR 60-119) reporting `soc`.
    fn lv_block(soc: u16, serial: &str) -> Vec<u16> {
        let mut data = vec![0u16; 60];
        data[23] = 50_000; // IR(83): 50.0 V in mV
        data[25] = 16_000; // IR(85): 160 Ah
        for (i, bytes) in serial.as_bytes().chunks_exact(2).enumerate() {
            data[50 + i] = u16::from_be_bytes([bytes[0], bytes[1]]);
        }
        data[40] = soc; // IR(100)
        data[43] = 250; // IR(103): 25.0 C
        data
    }

    /// A present HV BCU cluster block (IR 60-119) with `modules` modules.
    fn bcu_block(modules: u16) -> Vec<u16> {
        let mut data = vec![0u16; 60];
        data[0] = 0x4741; // "GA"
        data[1] = 0x3030; // "00"
        data[3] = 5; // -> "GA000005"
        data[4] = modules; // IR(64)
        data[5] = 24; // IR(65): cells per module
        data[13] = 3800; // IR(73): 380.0 V
        data[20] = (95 << 8) | 85; // IR(80): SOC max 95 / min 85
        data
    }

    /// A present HV BMU block: 24 cells at 3.2 V, serial at offset 54.
    fn bmu_block(serial: &str) -> Vec<u16> {
        let mut data = vec![0u16; 60];
        for cell in data.iter_mut().take(24) {
            *cell = 3200;
        }
        for t in data.iter_mut().skip(30).take(24) {
            *t = 250;
        }
        for (i, bytes) in serial.as_bytes().chunks_exact(2).enumerate() {
            data[54 + i] = u16::from_be_bytes([bytes[0], bytes[1]]);
        }
        data
    }

    fn bms_reporting(bcus: u16) -> Vec<u16> {
        let mut d = vec![0u16; 5];
        d[1] = bcus;
        d
    }

    fn module(soc: u8, cap_ah: f32, remaining_ah: f32) -> BatteryModule {
        BatteryModule {
            soc,
            capacity_ah: cap_ah,
            remaining_capacity_ah: remaining_ah,
            ..Default::default()
        }
    }

    // ---- SOC rules -----------------------------------------------------------------

    #[test]
    fn a_zero_inverter_soc_falls_back_to_a_believable_bms_soc() {
        let mut snap = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(55, 160.0, 88.0)],
            ..Default::default()
        };
        apply_bms_soc_fallback(&mut snap);
        assert_eq!(snap.soc, 55);
    }

    #[test]
    fn the_bms_fallback_leaves_a_real_inverter_soc_alone() {
        let mut snap = InverterSnapshot {
            soc: 42,
            battery_modules: vec![module(55, 160.0, 88.0)],
            ..Default::default()
        };
        apply_bms_soc_fallback(&mut snap);
        assert_eq!(snap.soc, 42);
    }

    #[test]
    fn the_bms_fallback_rejects_zero_and_full_and_missing_readings() {
        for bms_soc in [0u8, 100] {
            let mut snap = InverterSnapshot {
                soc: 0,
                battery_modules: vec![module(bms_soc, 160.0, 88.0)],
                ..Default::default()
            };
            apply_bms_soc_fallback(&mut snap);
            assert_eq!(
                snap.soc, 0,
                "BMS SOC {bms_soc} is not a believable fallback"
            );
        }
        let mut none = InverterSnapshot::default();
        apply_bms_soc_fallback(&mut none);
        assert_eq!(none.soc, 0);
    }

    #[test]
    fn a_zero_soc_becomes_the_capacity_weighted_average_of_all_modules() {
        // 50/100 Ah plus 100/100 Ah remaining: 150/200 = 75%, not the mean of
        // the two per-module percentages.
        let mut snap = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(50, 100.0, 50.0), module(100, 100.0, 100.0)],
            ..Default::default()
        };
        apply_aggregate_soc(&mut snap);
        assert_eq!(snap.soc, 75);

        let mut unequal = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(0, 100.0, 0.0), module(0, 300.0, 300.0)],
            ..Default::default()
        };
        apply_aggregate_soc(&mut unequal);
        assert_eq!(unequal.soc, 75, "weighted by capacity: 300/400");
    }

    #[test]
    fn the_aggregate_soc_is_capped_at_100() {
        let mut snap = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(0, 100.0, 120.0), module(0, 100.0, 120.0)],
            ..Default::default()
        };
        apply_aggregate_soc(&mut snap);
        assert_eq!(snap.soc, 100);
    }

    #[test]
    fn the_aggregate_soc_needs_several_modules_and_some_capacity() {
        let one = |s: &mut InverterSnapshot| apply_aggregate_soc(s);
        let mut single = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(50, 100.0, 50.0)],
            ..Default::default()
        };
        one(&mut single);
        assert_eq!(single.soc, 0, "one module is the BMS fallback's job");

        let mut no_capacity = InverterSnapshot {
            soc: 0,
            battery_modules: vec![module(50, 0.0, 0.0), module(50, 0.0, 0.0)],
            ..Default::default()
        };
        apply_aggregate_soc(&mut no_capacity);
        assert_eq!(no_capacity.soc, 0);

        let mut healthy = InverterSnapshot {
            soc: 60,
            battery_modules: vec![module(50, 100.0, 50.0), module(50, 100.0, 50.0)],
            ..Default::default()
        };
        apply_aggregate_soc(&mut healthy);
        assert_eq!(healthy.soc, 60, "a real inverter SOC is never overridden");
    }

    // ---- LV batteries ----------------------------------------------------------------

    fn lv_state() -> Arc<AppState> {
        Arc::new(AppState::new())
    }

    async fn lost(state: &Arc<AppState>, addr: u8) -> bool {
        state
            .alert_debounce
            .lock()
            .await
            .battery_connection_lost_confirmed(addr)
    }

    #[tokio::test]
    async fn a_single_lv_battery_is_decoded_into_the_snapshot() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(80, "BG1234G567"));
            let mut snap = InverterSnapshot::default();
            let mut known = Vec::new();

            read_lv_batteries(&mut reader, &state, &mut snap, &mut known).await;

            assert_eq!(snap.battery_modules.len(), 1);
            assert_eq!(snap.battery_modules[0].soc, 80);
            assert_eq!(snap.battery_modules[0].index, 0);
            assert!(known.is_empty(), "battery #1 is not tracked as additional");
            // Battery #1 first, then the first additional address, which does not answer.
            assert_eq!(reader.calls[0], (0x32, 60, 60));
            assert_eq!(reader.calls[1], (0x33, 60, 60));
        })
        .await;
    }

    #[tokio::test]
    async fn a_zero_inverter_soc_is_taken_from_the_first_lv_bms() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(64, "BG1234G567"));
            let mut snap = InverterSnapshot::default(); // soc 0
            read_lv_batteries(&mut reader, &state, &mut snap, &mut Vec::new()).await;
            assert_eq!(snap.soc, 64);
        })
        .await;
    }

    #[tokio::test]
    async fn additional_batteries_are_read_in_address_order_and_stop_at_the_first_gap() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader
                .ok(0x32, 60, lv_block(80, "BG1234G567"))
                .ok(0x33, 60, lv_block(70, "BG2222G222"))
                .ok(0x34, 60, lv_block(60, "BG3333G333"));
            // 0x35 does not answer; 0x36 and 0x37 must not even be asked.
            let mut snap = InverterSnapshot::default();
            let mut known = Vec::new();

            read_lv_batteries(&mut reader, &state, &mut snap, &mut known).await;

            let socs: Vec<u8> = snap.battery_modules.iter().map(|m| m.soc).collect();
            assert_eq!(socs, vec![80, 70, 60]);
            let indices: Vec<usize> = snap.battery_modules.iter().map(|m| m.index).collect();
            assert_eq!(indices, vec![0, 1, 2]);
            assert_eq!(known, vec![0x33, 0x34]);
            assert_eq!(reader.asked(0x35), 1);
            assert_eq!(reader.asked(0x36), 0, "the scan ends at the first gap");
            assert_eq!(reader.asked(0x37), 0);
        })
        .await;
    }

    #[tokio::test]
    async fn an_address_answering_with_invalid_data_also_ends_the_scan() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader
                .ok(0x32, 60, lv_block(80, "BG1234G567"))
                .ok(0x33, 60, lv_block(200, "BG2222G222")) // answers, but invalid
                .ok(0x34, 60, lv_block(60, "BG3333G333"));
            let mut snap = InverterSnapshot::default();
            read_lv_batteries(&mut reader, &state, &mut snap, &mut Vec::new()).await;
            assert_eq!(snap.battery_modules.len(), 1);
            assert_eq!(
                reader.asked(0x34),
                0,
                "nothing beyond the first bad address"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_battery_is_only_announced_the_first_time_it_is_seen() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut known = vec![0x33];
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(80, "BG1234G567")).ok(
                0x33,
                60,
                lv_block(70, "BG2222G222"),
            );
            let mut snap = InverterSnapshot::default();
            read_lv_batteries(&mut reader, &state, &mut snap, &mut known).await;
            assert_eq!(known, vec![0x33], "no duplicate entry");
        })
        .await;
    }

    #[tokio::test]
    async fn an_invalid_or_failed_battery_1_read_counts_towards_connection_loss() {
        with_isolated_config_dir_async(|| async {
            for case in ["invalid", "error"] {
                let state = lv_state();
                let mut reader = Scripted::default();
                match case {
                    "invalid" => {
                        // SOC 200 is not a valid BMS response.
                        reader.ok(0x32, 60, lv_block(200, "BG1234G567"));
                    }
                    _ => {
                        reader.fail(0x32, 60);
                    }
                }
                for n in 1..=BATTERY_CONNECTION_LOST_CONFIRM_CYCLES {
                    let mut snap = InverterSnapshot::default();
                    read_lv_batteries(&mut reader, &state, &mut snap, &mut Vec::new()).await;
                    assert!(snap.battery_modules.is_empty(), "{case}: nothing decoded");
                    assert_eq!(
                        lost(&state, 0x32).await,
                        n >= BATTERY_CONNECTION_LOST_CONFIRM_CYCLES,
                        "{case}: cycle {n}"
                    );
                }
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_good_battery_1_read_clears_the_loss() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut bad = Scripted::default();
            bad.fail(0x32, 60);
            for _ in 0..BATTERY_CONNECTION_LOST_CONFIRM_CYCLES {
                read_lv_batteries(
                    &mut bad,
                    &state,
                    &mut InverterSnapshot::default(),
                    &mut Vec::new(),
                )
                .await;
            }
            assert!(lost(&state, 0x32).await);

            let mut good = Scripted::default();
            good.ok(0x32, 60, lv_block(80, "BG1234G567"));
            read_lv_batteries(
                &mut good,
                &state,
                &mut InverterSnapshot::default(),
                &mut Vec::new(),
            )
            .await;
            assert!(!lost(&state, 0x32).await);
        })
        .await;
    }

    #[tokio::test]
    async fn a_known_additional_battery_that_goes_quiet_counts_as_lost() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut known = vec![0x33];
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(80, "BG1234G567")); // 0x33 silent
            for _ in 0..BATTERY_CONNECTION_LOST_CONFIRM_CYCLES {
                read_lv_batteries(
                    &mut reader,
                    &state,
                    &mut InverterSnapshot::default(),
                    &mut known,
                )
                .await;
            }
            assert!(lost(&state, 0x33).await);
        })
        .await;
    }

    #[tokio::test]
    async fn an_address_that_was_never_seen_is_never_counted_as_lost() {
        // The unused 0x33 slot of a single-battery system must not alarm.
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(80, "BG1234G567"));
            for _ in 0..(BATTERY_CONNECTION_LOST_CONFIRM_CYCLES * 2) {
                read_lv_batteries(
                    &mut reader,
                    &state,
                    &mut InverterSnapshot::default(),
                    &mut Vec::new(),
                )
                .await;
            }
            assert!(!lost(&state, 0x33).await);
            assert!(!state
                .alert_debounce
                .lock()
                .await
                .any_battery_connection_lost());
        })
        .await;
    }

    #[tokio::test]
    async fn a_known_battery_answering_with_invalid_data_counts_as_lost_but_a_stranger_does_not() {
        with_isolated_config_dir_async(|| async {
            let state = lv_state();
            let mut reader = Scripted::default();
            reader.ok(0x32, 60, lv_block(80, "BG1234G567")).ok(
                0x33,
                60,
                lv_block(200, "BG2222G222"),
            );
            let mut known = vec![0x33];
            for _ in 0..BATTERY_CONNECTION_LOST_CONFIRM_CYCLES {
                read_lv_batteries(
                    &mut reader,
                    &state,
                    &mut InverterSnapshot::default(),
                    &mut known,
                )
                .await;
            }
            assert!(lost(&state, 0x33).await, "known address, invalid data");

            let state2 = lv_state();
            for _ in 0..BATTERY_CONNECTION_LOST_CONFIRM_CYCLES {
                read_lv_batteries(
                    &mut reader,
                    &state2,
                    &mut InverterSnapshot::default(),
                    &mut Vec::new(),
                )
                .await;
            }
            assert!(!lost(&state2, 0x33).await, "never seen answer: not counted");
        })
        .await;
    }

    // ---- HV batteries -------------------------------------------------------------------

    fn hv() -> Option<DeviceType> {
        Some(DeviceType::HybridHvGen3)
    }

    #[tokio::test]
    async fn an_hv_stack_is_probed_then_read_into_modules_with_the_cluster_returned() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(3))
            .ok(0x50, 60, bmu_block("BM00000001"))
            .ok(0x51, 60, bmu_block("BM00000002"))
            .ok(0x52, 60, bmu_block("BM00000003"));
        let mut stacks = HvStacks::default();
        let mut snap = InverterSnapshot::default();

        let cluster = read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;

        assert_eq!(stacks.detected, vec![(0, 3)]);
        assert!(stacks.probe_done);
        assert_eq!(snap.battery_modules.len(), 3);
        let indices: Vec<usize> = snap.battery_modules.iter().map(|m| m.index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(snap.battery_modules[0].cell_voltages.len(), 24);
        let cluster = cluster.expect("the cluster read should be returned");
        assert_eq!(cluster.number_of_modules, 3);
        assert!((cluster.battery_voltage - 380.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn backfill_gives_hv_modules_a_soc_from_the_cluster() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(1))
            .ok(0x50, 60, bmu_block("BM00000001"));
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut HvStacks::default(), hv(), &mut snap, ZERO).await;
        assert_eq!(snap.battery_modules.len(), 1);
        assert!(
            snap.battery_modules[0].soc > 0,
            "the BMU has no SOC register; it must come from the cluster"
        );
    }

    #[tokio::test]
    async fn without_a_cluster_modules_are_left_unfilled() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(1))
            .ok(0x50, 60, bmu_block("BM00000001"));
        let mut stacks = HvStacks::default();
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;

        // Next cycle the BCU stops answering validly; modules still read, but
        // there is no cluster to backfill from.
        reader.ok(0x70, 60, vec![0u16; 60]);
        let mut snap2 = InverterSnapshot::default();
        let cluster = read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap2, ZERO).await;
        assert!(cluster.is_none());
        assert_eq!(snap2.battery_modules.len(), 1);
        assert_eq!(snap2.battery_modules[0].soc, 0);
    }

    #[tokio::test]
    async fn two_stacks_are_read_with_their_own_bmu_bases_and_continuous_module_indices() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(2))
            .ok(0x70, 60, bcu_block(2))
            .ok(0x71, 60, bcu_block(1))
            // Stack 0 reads its BMUs at base 60, stack 1 at base 180 (120 per
            // BCU offset); both address BMUs from 0x50.
            .ok(0x50, 60, bmu_block("BM0000000A"))
            .ok(0x51, 60, bmu_block("BM0000000B"))
            .ok(0x50, 180, bmu_block("BM0000000C"));
        let mut stacks = HvStacks::default();
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;

        assert_eq!(stacks.detected, vec![(0, 2), (1, 1)]);
        let serials: Vec<&str> = snap
            .battery_modules
            .iter()
            .map(|m| m.serial.as_str())
            .collect();
        assert_eq!(serials, vec!["BM0000000A", "BM0000000B", "BM0000000C"]);
        let indices: Vec<usize> = snap.battery_modules.iter().map(|m| m.index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
        assert!(reader.calls.contains(&(0x50, 180, 60)));
    }

    #[tokio::test]
    async fn the_first_cluster_wins_when_several_stacks_answer() {
        let mut first = bcu_block(1);
        first[13] = 3800; // 380.0 V
        let mut second = bcu_block(1);
        second[13] = 4100; // 410.0 V
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(2))
            .ok(0x70, 60, first)
            .ok(0x71, 60, second);
        let cluster = read_hv_battery(
            &mut reader,
            &mut HvStacks::default(),
            hv(),
            &mut InverterSnapshot::default(),
            ZERO,
        )
        .await
        .unwrap();
        assert!((cluster.battery_voltage - 380.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn a_missing_bmu_leaves_a_gap_in_the_module_indices() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(3))
            .ok(0x50, 60, bmu_block("BM00000001"))
            .ok(0x51, 60, vec![0u16; 60]) // no serial: not present
            .ok(0x52, 60, bmu_block("BM00000003"));
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut HvStacks::default(), hv(), &mut snap, ZERO).await;
        let indices: Vec<usize> = snap.battery_modules.iter().map(|m| m.index).collect();
        assert_eq!(indices, vec![0, 2], "the index is the module's position");
    }

    #[tokio::test]
    async fn a_silent_bmu_is_skipped_without_losing_the_rest() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(2))
            .fail(0x50, 60)
            .ok(0x51, 60, bmu_block("BM00000002"));
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut HvStacks::default(), hv(), &mut snap, ZERO).await;
        assert_eq!(snap.battery_modules.len(), 1);
        assert_eq!(snap.battery_modules[0].index, 1);
    }

    #[tokio::test]
    async fn when_the_bms_does_not_answer_a_single_stack_is_found_at_bcu_0x70() {
        let mut reader = Scripted::default();
        reader
            .fail(0xA0, 60)
            .ok(0x70, 60, bcu_block(2))
            .ok(0x50, 60, bmu_block("BM00000001"))
            .ok(0x51, 60, bmu_block("BM00000002"));
        let mut stacks = HvStacks::default();
        let mut snap = InverterSnapshot::default();
        read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;
        assert_eq!(stacks.detected, vec![(0, 2)]);
        assert!(stacks.probe_done);
        assert_eq!(snap.battery_modules.len(), 2);
    }

    #[tokio::test]
    async fn a_corrupt_bcu_count_is_capped_and_the_probe_stops_at_consecutive_gaps() {
        let mut reader = Scripted::default();
        reader.ok(0xA0, 60, bms_reporting(0xFFFF)); // no BCU answers at all
        let mut stacks = HvStacks::default();
        read_hv_battery(
            &mut reader,
            &mut stacks,
            hv(),
            &mut InverterSnapshot::default(),
            ZERO,
        )
        .await;
        let bcu_probes = reader
            .calls
            .iter()
            .filter(|c| (0x70..=0x8F).contains(&c.0))
            .count();
        assert!(
            bcu_probes < HV_MAX_BCU_COUNT as usize,
            "must stop early rather than walk every address: {bcu_probes}"
        );
        assert!(
            reader.calls.iter().all(|c| c.0 <= 0x8F || c.0 == 0xA0),
            "never probe outside the BCU address range"
        );
        assert!(stacks.detected.is_empty());
    }

    #[tokio::test]
    async fn an_empty_probe_is_retried_only_on_a_slow_cadence() {
        let mut reader = Scripted::default(); // nothing answers
        let mut stacks = HvStacks::default();
        let mut snap = InverterSnapshot::default();

        read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;
        assert!(stacks.probe_attempted && !stacks.probe_done);
        let first_probe_reads = reader.asked(0xA0);
        assert_eq!(first_probe_reads, 1);

        // The very next cycle must not probe again.
        read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;
        assert_eq!(reader.asked(0xA0), 1, "no BMS timeout on every poll");

        // But it does retry within a bounded number of cycles.
        for _ in 0..20 {
            read_hv_battery(&mut reader, &mut stacks, hv(), &mut snap, ZERO).await;
        }
        assert!(reader.asked(0xA0) > 1, "a startup timeout must recover");
    }

    #[tokio::test]
    async fn once_found_the_stacks_are_not_probed_again() {
        let mut reader = Scripted::default();
        reader
            .ok(0xA0, 60, bms_reporting(1))
            .ok(0x70, 60, bcu_block(1))
            .ok(0x50, 60, bmu_block("BM00000001"));
        let mut stacks = HvStacks::default();
        for _ in 0..8 {
            read_hv_battery(
                &mut reader,
                &mut stacks,
                hv(),
                &mut InverterSnapshot::default(),
                ZERO,
            )
            .await;
        }
        assert_eq!(reader.asked(0xA0), 1, "probed exactly once");
        assert!(reader.asked(0x70) >= 8, "the cluster is read every cycle");
    }

    #[tokio::test]
    async fn a_non_hv_device_never_probes() {
        let mut reader = Scripted::default();
        let mut stacks = HvStacks::default();
        read_hv_battery(
            &mut reader,
            &mut stacks,
            Some(DeviceType::Gen2Hybrid),
            &mut InverterSnapshot::default(),
            ZERO,
        )
        .await;
        assert!(reader.calls.is_empty());
        assert!(!stacks.probe_attempted);
    }

    // ---- external meters -----------------------------------------------------------------

    fn meter_block(volts_x10: u16) -> Vec<u16> {
        let mut d = vec![0u16; 30];
        d[0] = volts_x10;
        d
    }

    #[tokio::test]
    async fn each_detected_meter_is_read_and_cached() {
        let mut reader = Scripted::default();
        reader
            .ok(0x01, 60, meter_block(2300))
            .ok(0x02, 60, meter_block(2310));
        let mut cached = BTreeMap::new();
        let mut failures = BTreeMap::new();
        read_external_meters(&mut reader, &[0x01, 0x02], &mut cached, &mut failures, ZERO).await;
        assert_eq!(cached.len(), 2);
        assert!((cached[&0x01].v_phase_1 - 230.0).abs() < 0.01);
        assert!((cached[&0x02].v_phase_1 - 231.0).abs() < 0.01);
        assert_eq!(reader.calls, vec![(0x01, 60, 30), (0x02, 60, 30)]);
    }

    #[tokio::test]
    async fn no_meters_means_no_reads() {
        let mut reader = Scripted::default();
        read_external_meters(
            &mut reader,
            &[],
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            ZERO,
        )
        .await;
        assert!(reader.calls.is_empty());
    }

    #[tokio::test]
    async fn a_silent_meter_keeps_its_cached_reading_for_a_while_then_drops() {
        let mut reader = Scripted::default();
        reader.ok(0x01, 60, meter_block(2300));
        let mut cached = BTreeMap::new();
        let mut failures = BTreeMap::new();
        read_external_meters(&mut reader, &[0x01], &mut cached, &mut failures, ZERO).await;
        assert!(cached.contains_key(&0x01));

        reader.fail(0x01, 60);
        read_external_meters(&mut reader, &[0x01], &mut cached, &mut failures, ZERO).await;
        assert!(
            cached.contains_key(&0x01),
            "one missed read must not blank the meter"
        );
        assert_eq!(failures[&0x01], 1);

        for _ in 0..30 {
            read_external_meters(&mut reader, &[0x01], &mut cached, &mut failures, ZERO).await;
        }
        assert!(!cached.contains_key(&0x01), "a long-dead meter is dropped");
    }

    #[tokio::test]
    async fn a_meter_that_recovers_resets_its_failure_count() {
        let mut reader = Scripted::default();
        reader.fail(0x01, 60);
        let mut cached = BTreeMap::new();
        let mut failures = BTreeMap::new();
        for _ in 0..3 {
            read_external_meters(&mut reader, &[0x01], &mut cached, &mut failures, ZERO).await;
        }
        assert_eq!(failures[&0x01], 3);
        reader.ok(0x01, 60, meter_block(2300));
        read_external_meters(&mut reader, &[0x01], &mut cached, &mut failures, ZERO).await;
        assert!(!failures.contains_key(&0x01));
        assert!(cached.contains_key(&0x01));
    }

    #[tokio::test]
    async fn one_dead_meter_does_not_stop_the_others_being_read() {
        let mut reader = Scripted::default();
        reader.fail(0x01, 60).ok(0x02, 60, meter_block(2310));
        let mut cached = BTreeMap::new();
        read_external_meters(
            &mut reader,
            &[0x01, 0x02],
            &mut cached,
            &mut BTreeMap::new(),
            ZERO,
        )
        .await;
        assert!(cached.contains_key(&0x02));
    }
}
