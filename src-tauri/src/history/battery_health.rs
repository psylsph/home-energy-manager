//! Daily battery health summary (issue #393).
//!
//! One row per battery per local day in `battery_health_daily`, so a
//! failing cell or fading capacity shows as a trend over months. Nothing here
//! touches `readings`: that table is one row per minute for the whole
//! system, this one is one row per module per day.
//!
//! Polls fold into an in-memory [`HealthAccumulator`] and reach SQLite at most
//! every [`FLUSH_INTERVAL_SECS`] (and at a day change or on shutdown), merging
//! into the day's row with `ON CONFLICT`: extremes widen, counters add, the
//! latest value wins for slow-moving fields. Because the merge happens in SQL,
//! a restart mid-day continues the day's row instead of replacing it, and a
//! day's row is never deleted.
//!
//! A row is keyed by the slot the battery sat in and its serial (issue #427),
//! and [`HistoryDb::query_battery_health`] reports each battery under the slot
//! it sits in now, so adding a battery that takes over the master slot moves
//! nothing: the old battery's history follows it to its new slot. BMS serials
//! sometimes read back corrupt, so a slot's serial only changes once a new one
//! reads the same [`SERIAL_CONFIRM_READS`] polls running, and the polls that
//! read it before it settles are left out rather than filed under the old
//! battery. Until a slot has any confirmed serial (and for modules that never
//! report a usable one) the row's serial is empty and it belongs to the slot.
//!
//! Cell spread (max minus min cell voltage) depends heavily on state of
//! charge: cells drift apart near full and near empty even on a healthy pack.
//! Two spread figures are therefore kept: the day's worst (`max_spread_mv`)
//! and the worst while the module was nearly full (`full_spread_mv`), the
//! like-for-like figure to trend.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Local, NaiveDate};
use rusqlite::params;
use serde::Serialize;

use super::HistoryDb;
use crate::inverter::model::{BatteryModule, InverterSnapshot};
use crate::inverter::soc_watch::{SocEvent, SocEventKind, SocSource};

/// Longest a fold waits before asking for a flush to SQLite.
pub const FLUSH_INTERVAL_SECS: i64 = 300;

/// Module SOC at or above which a spread counts towards `full_spread_mv`.
pub const FULL_SOC_PERCENT: u8 = 95;

/// A cell reading outside this range is a failed read (zeros and 0xFFFF
/// fill), not a cell: the whole sample's cell figures are skipped. The lower
/// bound is deliberately far below any live cell so a genuinely collapsing
/// cell is still recorded; the upper bound is above any chemistry's full
/// charge (LFP ~3.65 V, NMC 4.2 V), so a corrupt high read cannot set the
/// day's maximum.
const CELL_VOLTS_VALID: std::ops::RangeInclusive<f32> = 0.5..=4.5;

/// The most charge cycles a battery can plausibly report; a 0xFFFF fill or
/// any larger value is a corrupt read.
const CYCLES_MAX: u16 = 20_000;

/// The largest plausible module capacity in Ah (uint32 fill reads as millions).
const CAPACITY_AH_MAX: f32 = 1_000.0;

/// Module temperatures outside this range are corrupt reads.
const TEMP_C_VALID: std::ops::RangeInclusive<f32> = -40.0..=100.0;

/// The lowest number of cells a spread is meaningful for.
const MIN_CELLS_FOR_SPREAD: usize = 2;

/// Consecutive polls a new serial must read back identically before a slot
/// adopts it, so a corrupt read can't make a battery look like a new one.
pub const SERIAL_CONFIRM_READS: u8 = 3;

/// A battery serial is ten alphanumeric characters; anything outside this
/// shape is a failed read (zeros, 0xFFFF fill, a half-read register).
fn plausible_serial(serial: &str) -> Option<&str> {
    let serial = serial.trim();
    ((6..=20).contains(&serial.len()) && serial.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(serial)
}

/// The serial each slot has settled on, for telling apart a battery that
/// moved slot from one that is still being read.
#[derive(Default)]
struct SlotSerial {
    confirmed: Option<String>,
    candidate: Option<String>,
    streak: u8,
    /// The last poll read a serial that hasn't settled, so it was left out.
    unsettled: bool,
}

impl SlotSerial {
    /// Take one poll's serial in. Returns whether the poll must be left out:
    /// it reads a serial other than the one this slot has settled on, and that
    /// one hasn't settled yet, so the poll belongs to no battery we can name.
    fn read(&mut self, serial: Option<&str>) -> bool {
        let Some(serial) = serial else {
            self.unsettled = false;
            return false;
        };
        if self.candidate.as_deref() == Some(serial) {
            self.streak = self.streak.saturating_add(1);
        } else {
            self.candidate = Some(serial.to_string());
            self.streak = 1;
        }
        if self.streak >= SERIAL_CONFIRM_READS && self.confirmed.as_deref() != Some(serial) {
            self.confirmed = Some(serial.to_string());
        }
        self.unsettled = self.confirmed.as_deref().is_some_and(|c| c != serial);
        self.unsettled
    }

    fn key(&self) -> String {
        self.confirmed.clone().unwrap_or_default()
    }
}

/// One module's summary for one local day.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatteryHealthRow {
    /// Local date, `YYYY-MM-DD`.
    pub day: String,
    /// Module slot (0-based), as in the snapshot's `battery_modules`. In the
    /// table this is the slot the battery sat in that day; a query reports the
    /// slot it sits in now.
    pub module_index: i64,
    /// The serial confirmed for the battery that day; empty when none was.
    /// Storage detail, not part of the API.
    #[serde(skip)]
    pub serial: String,
    pub min_cell_v: Option<f64>,
    pub max_cell_v: Option<f64>,
    /// Largest max-minus-min cell voltage seen that day, in mV.
    pub max_spread_mv: Option<f64>,
    /// Largest spread seen while the module was at or above
    /// [`FULL_SOC_PERCENT`], in mV.
    pub full_spread_mv: Option<f64>,
    pub cycles: Option<i64>,
    pub max_temp_c: Option<f64>,
    /// BMS-reported calibrated capacity (Ah).
    pub capacity_ah: Option<f64>,
    pub design_capacity_ah: Option<f64>,
    pub bms_firmware: Option<i64>,
    /// How many polls contributed.
    pub samples: i64,
    /// SOC jumps confirmed that day (see `inverter::soc_watch`).
    pub soc_jumps: i64,
    /// Stuck-SOC episodes confirmed that day.
    pub soc_stuck: i64,
}

impl BatteryHealthRow {
    fn new(day: &str, module_index: i64, serial: &str) -> Self {
        Self {
            day: day.to_string(),
            module_index,
            serial: serial.to_string(),
            min_cell_v: None,
            max_cell_v: None,
            max_spread_mv: None,
            full_spread_mv: None,
            cycles: None,
            max_temp_c: None,
            capacity_ah: None,
            design_capacity_ah: None,
            bms_firmware: None,
            samples: 0,
            soc_jumps: 0,
            soc_stuck: 0,
        }
    }

    /// Merge an earlier window of the same day into this one: extremes widen,
    /// samples add, and this (newer) window's slow-moving values win.
    fn absorb(&mut self, earlier: &BatteryHealthRow) {
        fn lower(a: Option<f64>, b: Option<f64>) -> Option<f64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        fn higher(a: Option<f64>, b: Option<f64>) -> Option<f64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            }
        }
        self.min_cell_v = lower(self.min_cell_v, earlier.min_cell_v);
        self.max_cell_v = higher(self.max_cell_v, earlier.max_cell_v);
        self.max_spread_mv = higher(self.max_spread_mv, earlier.max_spread_mv);
        self.full_spread_mv = higher(self.full_spread_mv, earlier.full_spread_mv);
        self.max_temp_c = higher(self.max_temp_c, earlier.max_temp_c);
        self.cycles = match (self.cycles, earlier.cycles) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.capacity_ah = self.capacity_ah.or(earlier.capacity_ah);
        self.design_capacity_ah = self.design_capacity_ah.or(earlier.design_capacity_ah);
        self.bms_firmware = self.bms_firmware.or(earlier.bms_firmware);
        self.samples += earlier.samples;
        self.soc_jumps += earlier.soc_jumps;
        self.soc_stuck += earlier.soc_stuck;
    }

    fn fold(&mut self, module: &BatteryModule, effective_soc: u8) {
        self.samples += 1;

        let cells = &module.cell_voltages;
        if cells.len() >= MIN_CELLS_FOR_SPREAD && cells.iter().all(|v| CELL_VOLTS_VALID.contains(v))
        {
            let lo = cells.iter().copied().fold(f32::INFINITY, f32::min) as f64;
            let hi = cells.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
            let spread_mv = (hi - lo) * 1000.0;
            self.min_cell_v = Some(self.min_cell_v.map_or(lo, |v| v.min(lo)));
            self.max_cell_v = Some(self.max_cell_v.map_or(hi, |v| v.max(hi)));
            self.max_spread_mv = Some(self.max_spread_mv.map_or(spread_mv, |v| v.max(spread_mv)));
            if effective_soc >= FULL_SOC_PERCENT {
                self.full_spread_mv =
                    Some(self.full_spread_mv.map_or(spread_mv, |v| v.max(spread_mv)));
            }
        }
        if module.num_cycles > 0 && module.num_cycles <= CYCLES_MAX {
            let cycles = i64::from(module.num_cycles);
            self.cycles = Some(self.cycles.map_or(cycles, |v| v.max(cycles)));
        }
        if TEMP_C_VALID.contains(&module.temperature) && module.temperature.is_finite() {
            let temp = f64::from(module.temperature);
            self.max_temp_c = Some(self.max_temp_c.map_or(temp, |v| v.max(temp)));
        }
        // Capacity is the BMS's own calibrated figure, which only an LV BMS
        // reports (it also reports a firmware). HV stacks give every module
        // the BCU's nominal Ah as both capacity and design, which would read
        // as 100% for ever and hide real fade.
        if module.bms_firmware > 0 {
            self.bms_firmware = Some(i64::from(module.bms_firmware));
            if (0.0..=CAPACITY_AH_MAX).contains(&module.capacity_ah) && module.capacity_ah > 0.0 {
                self.capacity_ah = Some(f64::from(module.capacity_ah));
            }
            if (0.0..=CAPACITY_AH_MAX).contains(&module.design_capacity_ah)
                && module.design_capacity_ah > 0.0
            {
                self.design_capacity_ah = Some(f64::from(module.design_capacity_ah));
            }
        }
    }
}

/// Polls since the last flush, per (day, module slot, serial).
#[derive(Default)]
pub(super) struct HealthAccumulator {
    rows: HashMap<(String, i64, String), BatteryHealthRow>,
    serials: HashMap<i64, SlotSerial>,
    /// Unix seconds of the last flush request, 0 before the first.
    last_flush_ts: i64,
}

impl HealthAccumulator {
    /// An accumulator that starts out expecting each slot to hold the battery
    /// last recorded in it, so a restart's first polls land on that battery's
    /// row instead of waiting for the serial to settle again. A read that
    /// fails leaves it empty: the serials then settle from the polls.
    pub(super) fn seeded(conn: &rusqlite::Connection) -> Self {
        let mut accumulator = Self::default();
        let loaded = SerialOwners::load(conn);
        match loaded {
            Ok(owners) => {
                for (slot, history) in owners.by_slot {
                    if let Some((_, serial)) = history.last() {
                        accumulator.serials.insert(
                            slot,
                            SlotSerial {
                                confirmed: Some(serial.clone()),
                                ..SlotSerial::default()
                            },
                        );
                    }
                }
            }
            Err(e) => tracing::warn!("Could not read the batteries' serials: {e}"),
        }
        accumulator
    }

    /// Fold one poll in. Returns whether a flush is now due: the first poll,
    /// [`FLUSH_INTERVAL_SECS`] since the last, a clock that moved backwards,
    /// or rows held for a day that has ended.
    pub(super) fn observe(&mut self, snap: &InverterSnapshot, now: DateTime<Local>) -> bool {
        let today = now.format("%Y-%m-%d").to_string();
        for module in &snap.battery_modules {
            let slot = module.index as i64;
            // A serial two modules report is no identity for either.
            let serial = plausible_serial(&module.serial).filter(|serial| {
                snap.battery_modules
                    .iter()
                    .filter(|other| plausible_serial(&other.serial) == Some(*serial))
                    .count()
                    == 1
            });
            let slot_serial = self.serials.entry(slot).or_default();
            if slot_serial.read(serial) {
                tracing::debug!(slot, "Battery health poll left out: serial not settled yet");
                continue;
            }
            let serial = slot_serial.key();
            let effective_soc = if module.soc > 0 { module.soc } else { snap.soc };
            self.rows
                .entry((today.clone(), slot, serial.clone()))
                .or_insert_with(|| BatteryHealthRow::new(&today, slot, &serial))
                .fold(module, effective_soc);
        }
        if self.rows.is_empty() {
            return false;
        }
        let now_ts = now.timestamp();
        let day_ended = self.rows.keys().any(|(day, _, _)| *day != today);
        let elapsed = now_ts - self.last_flush_ts;
        if day_ended || !(0..FLUSH_INTERVAL_SECS).contains(&elapsed) {
            self.last_flush_ts = now_ts;
            return true;
        }
        false
    }

    /// Put rows back after a failed write, merged with anything folded since,
    /// so a transient failure loses nothing. Mirrors the SQL merge.
    fn restore(&mut self, rows: Vec<BatteryHealthRow>) {
        for row in rows {
            let key = (row.day.clone(), row.module_index, row.serial.clone());
            match self.rows.get_mut(&key) {
                Some(held) => held.absorb(&row),
                None => {
                    self.rows.insert(key, row);
                }
            }
        }
    }

    fn count_soc_event(&mut self, day: &str, slot: i64, kind: &SocEventKind) {
        // An event about a battery whose serial hasn't settled can't be
        // placed on the old battery's row, nor yet on a new one.
        if self.serials.get(&slot).is_some_and(|s| s.unsettled) {
            return;
        }
        let serial = self
            .serials
            .get(&slot)
            .map(SlotSerial::key)
            .unwrap_or_default();
        let row = self
            .rows
            .entry((day.to_string(), slot, serial.clone()))
            .or_insert_with(|| BatteryHealthRow::new(day, slot, &serial));
        match kind {
            SocEventKind::Jump { .. } => row.soc_jumps += 1,
            SocEventKind::Stuck { .. } => row.soc_stuck += 1,
        }
    }

    fn take(&mut self) -> Vec<BatteryHealthRow> {
        self.rows.drain().map(|(_, row)| row).collect()
    }
}

const UPSERT_SQL: &str = "\
INSERT INTO battery_health_daily \
 (day, module_index, min_cell_v, max_cell_v, max_spread_mv, full_spread_mv, \
  cycles, max_temp_c, capacity_ah, design_capacity_ah, bms_firmware, samples, updated_at, \
  soc_jumps, soc_stuck, serial) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
ON CONFLICT(day, module_index, serial) DO UPDATE SET \
 min_cell_v = MIN(COALESCE(min_cell_v, excluded.min_cell_v), COALESCE(excluded.min_cell_v, min_cell_v)), \
 max_cell_v = MAX(COALESCE(max_cell_v, excluded.max_cell_v), COALESCE(excluded.max_cell_v, max_cell_v)), \
 max_spread_mv = MAX(COALESCE(max_spread_mv, excluded.max_spread_mv), COALESCE(excluded.max_spread_mv, max_spread_mv)), \
 full_spread_mv = MAX(COALESCE(full_spread_mv, excluded.full_spread_mv), COALESCE(excluded.full_spread_mv, full_spread_mv)), \
 cycles = MAX(COALESCE(cycles, excluded.cycles), COALESCE(excluded.cycles, cycles)), \
 max_temp_c = MAX(COALESCE(max_temp_c, excluded.max_temp_c), COALESCE(excluded.max_temp_c, max_temp_c)), \
 capacity_ah = COALESCE(excluded.capacity_ah, capacity_ah), \
 design_capacity_ah = COALESCE(excluded.design_capacity_ah, design_capacity_ah), \
 bms_firmware = COALESCE(excluded.bms_firmware, bms_firmware), \
 samples = samples + excluded.samples, \
 soc_jumps = soc_jumps + excluded.soc_jumps, \
 soc_stuck = soc_stuck + excluded.soc_stuck, \
 updated_at = excluded.updated_at";

impl HistoryDb {
    /// Fold a live poll into the day's battery health summary. Cheap and
    /// non-blocking. Returns whether a [`Self::flush_battery_health`] on the
    /// blocking pool is now due.
    pub fn observe_battery_health(&self, snap: &InverterSnapshot, now: DateTime<Local>) -> bool {
        // The accumulator holds nothing that matters for correctness, so a
        // poisoned lock (a panic mid-fold) is recovered rather than stopping
        // the summary for good.
        self.health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(snap, now)
    }

    /// Count confirmed SOC jumps and stuck episodes on today's rows. A
    /// system-SOC event (no module reports its own SOC) is about the whole
    /// battery, so it counts once, on the lowest module slot in `snap`.
    pub fn record_soc_events(
        &self,
        events: &[SocEvent],
        snap: &InverterSnapshot,
        now: DateTime<Local>,
    ) {
        if events.is_empty() {
            return;
        }
        let today = now.format("%Y-%m-%d").to_string();
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        for event in events {
            let slot = match event.source {
                SocSource::Module(index) => Some(index as i64),
                SocSource::System => snap.battery_modules.iter().map(|m| m.index as i64).min(),
            };
            if let Some(slot) = slot {
                health.count_soc_event(&today, slot, &event.kind);
            }
        }
    }

    /// Merge the accumulated polls into `battery_health_daily`. Blocking
    /// (SQLite). The batch is one transaction, so a failure leaves the table
    /// as it was; the polls it held are dropped, which costs only that
    /// window's extremes.
    pub fn flush_battery_health(&self) {
        let mut conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                let dropped = self.health.lock().unwrap_or_else(|e| e.into_inner()).take();
                tracing::warn!(
                    "History DB lock poisoned, dropped {} battery health row(s): {e}",
                    dropped.len()
                );
                return;
            }
        };
        let rows = self.health.lock().unwrap_or_else(|e| e.into_inner()).take();
        if rows.is_empty() {
            return;
        }
        let updated_at = chrono::Utc::now().timestamp();
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(UPSERT_SQL)?;
                for row in &rows {
                    stmt.execute(params![
                        row.day,
                        row.module_index,
                        row.min_cell_v,
                        row.max_cell_v,
                        row.max_spread_mv,
                        row.full_spread_mv,
                        row.cycles,
                        row.max_temp_c,
                        row.capacity_ah,
                        row.design_capacity_ah,
                        row.bms_firmware,
                        row.samples,
                        updated_at,
                        row.soc_jumps,
                        row.soc_stuck,
                        row.serial,
                    ])?;
                }
            }
            tx.commit()
        })();
        if let Err(e) = result {
            tracing::warn!(
                "Failed to store {} battery health row(s), keeping them for the next flush: {e}",
                rows.len()
            );
            // Nothing was written (one transaction), so put the window back;
            // the merge tolerates it arriving with later polls.
            self.health
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .restore(rows);
        }
    }

    /// Daily summaries with `since <= day <= until`, oldest day first, then
    /// by module slot. Each battery is reported under the slot it sits in now
    /// (issue #427): a row recorded under a serial moves to that serial's
    /// latest slot, and a row with no serial (history from before serials
    /// were kept, or reads that never settled) goes with the battery that
    /// reported a serial in its slot nearest in time. Rows that land on one
    /// (day, slot) are merged.
    pub fn query_battery_health(
        &self,
        since: NaiveDate,
        until: NaiveDate,
    ) -> Result<Vec<BatteryHealthRow>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("History DB lock poisoned: {e}"))?;
        let owners = SerialOwners::load(&conn)?;
        let mut stmt = conn
            .prepare(
                "SELECT day, module_index, min_cell_v, max_cell_v, max_spread_mv, \
                 full_spread_mv, cycles, max_temp_c, capacity_ah, design_capacity_ah, \
                 bms_firmware, samples, soc_jumps, soc_stuck, serial \
                 FROM battery_health_daily WHERE day >= ?1 AND day <= ?2 \
                 ORDER BY day ASC, updated_at ASC, module_index ASC",
            )
            .map_err(|e| format!("Failed to prepare battery health query: {e}"))?;
        let rows = stmt
            .query_map(
                params![
                    since.format("%Y-%m-%d").to_string(),
                    until.format("%Y-%m-%d").to_string()
                ],
                |row| {
                    Ok(BatteryHealthRow {
                        day: row.get(0)?,
                        module_index: row.get(1)?,
                        min_cell_v: row.get(2)?,
                        max_cell_v: row.get(3)?,
                        max_spread_mv: row.get(4)?,
                        full_spread_mv: row.get(5)?,
                        cycles: row.get(6)?,
                        max_temp_c: row.get(7)?,
                        capacity_ah: row.get(8)?,
                        design_capacity_ah: row.get(9)?,
                        bms_firmware: row.get(10)?,
                        samples: row.get(11)?,
                        soc_jumps: row.get(12)?,
                        soc_stuck: row.get(13)?,
                        serial: row.get(14)?,
                    })
                },
            )
            .map_err(|e| format!("Battery health query failed: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Failed to read battery health row: {e}"))?;

        // Rows arrive oldest first, so a later row absorbs an earlier one.
        let mut merged: BTreeMap<(String, i64), BatteryHealthRow> = BTreeMap::new();
        for mut row in rows {
            row.module_index = owners.slot_now(&row.day, row.module_index, &row.serial);
            let key = (row.day.clone(), row.module_index);
            if let Some(earlier) = merged.remove(&key) {
                row.absorb(&earlier);
            }
            merged.insert(key, row);
        }
        Ok(merged.into_values().collect())
    }
}

/// Which serial sat in which slot, from every row that has a serial (the whole
/// table, not the queried range: a battery's current slot can lie outside it).
struct SerialOwners {
    /// Each serial's slot in its newest row.
    latest_slot: HashMap<String, i64>,
    /// Per slot, its serial-bearing rows as (day, serial), oldest first.
    by_slot: HashMap<i64, Vec<(String, String)>>,
}

impl SerialOwners {
    fn load(conn: &rusqlite::Connection) -> Result<Self, String> {
        let mut stmt = conn
            .prepare(
                "SELECT day, module_index, serial FROM battery_health_daily \
                 WHERE serial != '' ORDER BY day ASC, updated_at ASC, module_index ASC",
            )
            .map_err(|e| format!("Failed to prepare battery serial query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("Battery serial query failed: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Failed to read battery serial row: {e}"))?;
        let mut owners = Self {
            latest_slot: HashMap::new(),
            by_slot: HashMap::new(),
        };
        // The order each serial was last seen in, and the days it was seen.
        let mut last_seen: HashMap<String, usize> = HashMap::new();
        let mut days: HashMap<String, HashSet<String>> = HashMap::new();
        for (position, (day, slot, serial)) in rows.into_iter().enumerate() {
            owners.latest_slot.insert(serial.clone(), slot);
            last_seen.insert(serial.clone(), position);
            days.entry(serial.clone()).or_default().insert(day.clone());
            owners.by_slot.entry(slot).or_default().push((day, serial));
        }
        owners.retire_blended(&last_seen, &days);
        Ok(owners)
    }

    /// Two batteries can end up claiming one slot: a replacement (never there
    /// together, so one continuous line, as it always was), or a battery
    /// removed while another slid into its slot. The latter were both present
    /// on the same days, so sharing a slot would blend two packs into one
    /// line: the one seen longest ago is moved to a spare slot instead.
    fn retire_blended(
        &mut self,
        last_seen: &HashMap<String, usize>,
        days: &HashMap<String, HashSet<String>>,
    ) {
        let mut claims: BTreeMap<i64, Vec<&String>> = BTreeMap::new();
        for (serial, slot) in &self.latest_slot {
            claims.entry(*slot).or_default().push(serial);
        }
        // Past every slot a battery was ever in, so a retired line never
        // lands on one a live battery's history uses.
        let mut spare = self.by_slot.keys().max().map_or(0, |max| max + 1);
        let mut retired: Vec<(usize, String)> = Vec::new();
        for serials in claims.values().filter(|s| s.len() > 1) {
            let Some(keeper) = serials.iter().max_by_key(|s| last_seen[**s]) else {
                continue;
            };
            for serial in serials.iter().filter(|s| *s != keeper) {
                if !days[*serial].is_disjoint(&days[*keeper]) {
                    retired.push((last_seen[*serial], (*serial).clone()));
                }
            }
        }
        retired.sort();
        for (_, serial) in retired {
            self.latest_slot.insert(serial, spare);
            spare += 1;
        }
    }

    /// The slot the battery behind a row sits in now. A row with a serial
    /// follows it; one without goes with the newest serial its slot had by
    /// that day, else the first it ever had (history from before serials were
    /// kept), else stays where it is.
    fn slot_now(&self, day: &str, slot: i64, serial: &str) -> i64 {
        let serial = if serial.is_empty() {
            let Some(history) = self.by_slot.get(&slot) else {
                return slot;
            };
            // `history` is oldest first: the newest entry on or before `day`.
            let on_or_before = history.partition_point(|(seen, _)| seen.as_str() <= day);
            history
                .get(on_or_before.saturating_sub(1))
                .map(|(_, serial)| serial.as_str())
                .unwrap_or_default()
        } else {
            serial
        };
        self.latest_slot.get(serial).copied().unwrap_or(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("unambiguous local time")
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn module(index: usize, cells: &[f32], soc: u8) -> BatteryModule {
        BatteryModule {
            index,
            soc,
            temperature: 21.5,
            serial: format!("BAT{index}"),
            num_cycles: 120,
            num_cells: cells.len() as u16,
            cell_voltages: cells.to_vec(),
            bms_firmware: 3015,
            capacity_ah: 95.0,
            design_capacity_ah: 100.0,
            ..BatteryModule::default()
        }
    }

    fn snap(modules: Vec<BatteryModule>) -> InverterSnapshot {
        InverterSnapshot {
            soc: 60,
            battery_modules: modules,
            ..InverterSnapshot::default()
        }
    }

    fn test_db() -> HistoryDb {
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        HistoryDb::open(&dir.join("h.db")).unwrap()
    }

    fn record(db: &HistoryDb, s: &InverterSnapshot, now: DateTime<Local>) {
        db.observe_battery_health(s, now);
        db.flush_battery_health();
    }

    fn all_rows(db: &HistoryDb) -> Vec<BatteryHealthRow> {
        db.query_battery_health(day(2000, 1, 1), day(2100, 1, 1))
            .unwrap()
    }

    #[test]
    fn records_spread_extremes_cycles_and_capacity() {
        let db = test_db();
        let s = snap(vec![module(0, &[3.300, 3.310, 3.290, 3.305], 60)]);
        record(&db, &s, at(2026, 10, 1, 12, 0));

        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.day, "2026-10-01");
        assert_eq!(r.module_index, 0);
        assert!((r.min_cell_v.unwrap() - 3.290).abs() < 1e-4);
        assert!((r.max_cell_v.unwrap() - 3.310).abs() < 1e-4);
        assert!((r.max_spread_mv.unwrap() - 20.0).abs() < 0.1);
        assert_eq!(r.full_spread_mv, None, "module at 60% is not nearly full");
        assert_eq!(r.cycles, Some(120));
        assert_eq!(r.max_temp_c.map(|t| t.round()), Some(22.0));
        assert_eq!(r.capacity_ah.map(|c| c.round()), Some(95.0));
        assert_eq!(r.design_capacity_ah.map(|c| c.round()), Some(100.0));
        assert_eq!(r.bms_firmware, Some(3015));
        assert_eq!(r.samples, 1);
    }

    #[test]
    fn full_spread_only_counts_when_nearly_full() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        record(&db, &snap(vec![module(0, &[3.30, 3.40], 50)]), now);
        record(&db, &snap(vec![module(0, &[3.40, 3.43], 97)]), now);
        let r = &all_rows(&db)[0];
        assert!((r.max_spread_mv.unwrap() - 100.0).abs() < 0.1);
        assert!((r.full_spread_mv.unwrap() - 30.0).abs() < 0.1);
    }

    #[test]
    fn module_without_soc_uses_the_system_soc() {
        // HV BMU modules carry no SOC of their own.
        let db = test_db();
        let mut s = snap(vec![module(0, &[3.40, 3.42], 0)]);
        s.soc = 98;
        record(&db, &s, at(2026, 10, 1, 12, 0));
        assert!((all_rows(&db)[0].full_spread_mv.unwrap() - 20.0).abs() < 0.1);
    }

    #[test]
    fn a_day_merges_across_flushes_keeping_extremes() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        record(&db, &snap(vec![module(0, &[3.30, 3.32], 60)]), now);
        record(&db, &snap(vec![module(0, &[3.25, 3.40], 60)]), now);
        record(&db, &snap(vec![module(0, &[3.31, 3.33], 60)]), now);

        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1, "one row per module per day");
        let r = &rows[0];
        assert!((r.min_cell_v.unwrap() - 3.25).abs() < 1e-4);
        assert!((r.max_cell_v.unwrap() - 3.40).abs() < 1e-4);
        assert!((r.max_spread_mv.unwrap() - 150.0).abs() < 0.1);
        assert_eq!(r.samples, 3);
    }

    #[test]
    fn a_restart_continues_the_days_row() {
        // A second HistoryDb on the same file is a restarted process.
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-restart-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = dir.join("h.db");
        let now = at(2026, 10, 1, 9, 0);
        {
            let db = HistoryDb::open(&path).unwrap();
            record(&db, &snap(vec![module(0, &[3.20, 3.30], 60)]), now);
        }
        let db = HistoryDb::open(&path).unwrap();
        record(&db, &snap(vec![module(0, &[3.31, 3.32], 60)]), now);

        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1);
        assert!(
            (rows[0].max_spread_mv.unwrap() - 100.0).abs() < 0.1,
            "pre-restart extreme must survive"
        );
        assert_eq!(rows[0].samples, 2);
    }

    #[test]
    fn days_and_modules_get_their_own_rows() {
        let db = test_db();
        let s = snap(vec![
            module(0, &[3.30, 3.31], 60),
            module(1, &[3.30, 3.32], 60),
        ]);
        record(&db, &s, at(2026, 10, 1, 12, 0));
        record(&db, &s, at(2026, 10, 2, 12, 0));

        let rows = all_rows(&db);
        let keys: Vec<(&str, i64)> = rows
            .iter()
            .map(|r| (r.day.as_str(), r.module_index))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("2026-10-01", 0),
                ("2026-10-01", 1),
                ("2026-10-02", 0),
                ("2026-10-02", 1),
            ]
        );
    }

    #[test]
    fn failed_cell_reads_do_not_record_a_spread() {
        let db = test_db();
        // A zeroed cell is a failed read, not a 3 V spread.
        record(
            &db,
            &snap(vec![module(0, &[3.30, 0.0, 3.31], 60)]),
            at(2026, 10, 1, 12, 0),
        );
        let r = &all_rows(&db)[0];
        assert_eq!(r.max_spread_mv, None);
        assert_eq!(r.min_cell_v, None);
        assert_eq!(r.cycles, Some(120), "other fields still record");
        assert_eq!(r.samples, 1);
    }

    #[test]
    fn a_bad_read_does_not_erase_an_earlier_good_spread() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        record(&db, &snap(vec![module(0, &[3.30, 3.32], 60)]), now);
        record(&db, &snap(vec![module(0, &[0.0, 0.0], 60)]), now);
        let r = &all_rows(&db)[0];
        assert!((r.max_spread_mv.unwrap() - 20.0).abs() < 0.1);
    }

    #[test]
    fn corrupt_reads_inside_the_old_loose_bounds_are_not_recorded() {
        // The merge keeps extremes, so one corrupt value would stay for the
        // day: a 4.9 V cell (no live cell reads that), a 0xFFFF cycle count,
        // an absurd capacity.
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        let mut bad = module(0, &[3.30, 4.90, 3.31], 60);
        bad.num_cycles = 65_535;
        bad.capacity_ah = 42_949_672.0;
        bad.design_capacity_ah = 42_949_672.0;
        record(&db, &snap(vec![bad]), now);
        let r = &all_rows(&db)[0];
        assert_eq!(r.max_spread_mv, None);
        assert_eq!(r.max_cell_v, None);
        assert_eq!(r.cycles, None);
        assert_eq!(r.capacity_ah, None);
        assert_eq!(r.design_capacity_ah, None);

        // And a good read afterwards is not out-ranked by the bad one.
        record(&db, &snap(vec![module(0, &[3.30, 3.32], 60)]), now);
        let r = &all_rows(&db)[0];
        assert!((r.max_spread_mv.unwrap() - 20.0).abs() < 0.1);
        assert_eq!(r.cycles, Some(120));
    }

    #[test]
    fn a_failed_flush_keeps_the_rows_for_the_next_one() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        record(&db, &snap(vec![module(0, &[3.30, 3.40], 60)]), now);

        // The write fails (here: the table is missing).
        db.conn
            .lock()
            .unwrap()
            .execute_batch("ALTER TABLE battery_health_daily RENAME TO parked")
            .unwrap();
        record(&db, &snap(vec![module(0, &[3.20, 3.50], 60)]), now);
        record(&db, &snap(vec![module(0, &[3.30, 3.31], 60)]), now);

        // The table comes back: what was held across the failure is merged in.
        db.conn
            .lock()
            .unwrap()
            .execute_batch("ALTER TABLE parked RENAME TO battery_health_daily")
            .unwrap();
        db.flush_battery_health();

        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1);
        assert!(
            (rows[0].max_spread_mv.unwrap() - 300.0).abs() < 0.5,
            "the failed window's extreme must survive: {:?}",
            rows[0]
        );
        assert_eq!(rows[0].samples, 3, "no sample lost or counted twice");
    }

    fn soc_jump(source: SocSource) -> SocEvent {
        SocEvent {
            source,
            kind: SocEventKind::Jump { from: 60, to: 20 },
        }
    }

    fn soc_stuck(source: SocSource) -> SocEvent {
        SocEvent {
            source,
            kind: SocEventKind::Stuck {
                soc: 60,
                energy_kwh: 1.5,
            },
        }
    }

    fn counts(db: &HistoryDb) -> Vec<(String, i64, i64, i64)> {
        all_rows(db)
            .into_iter()
            .map(|r| (r.day, r.module_index, r.soc_jumps, r.soc_stuck))
            .collect()
    }

    #[test]
    fn soc_events_are_counted_per_module() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        let s = snap(vec![
            module(0, &[3.3, 3.31], 60),
            module(1, &[3.3, 3.31], 60),
        ]);
        db.observe_battery_health(&s, now);
        db.record_soc_events(
            &[
                soc_jump(SocSource::Module(0)),
                soc_jump(SocSource::Module(0)),
                soc_stuck(SocSource::Module(1)),
            ],
            &s,
            now,
        );
        db.flush_battery_health();
        assert_eq!(
            counts(&db),
            vec![
                ("2026-10-01".to_string(), 0, 2, 0),
                ("2026-10-01".to_string(), 1, 0, 1),
            ]
        );
    }

    #[test]
    fn a_system_soc_event_counts_once_on_the_first_module() {
        // One battery-level event must not read as one per module.
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        let s = snap(vec![module(1, &[3.3, 3.31], 0), module(0, &[3.3, 3.31], 0)]);
        db.observe_battery_health(&s, now);
        db.record_soc_events(&[soc_stuck(SocSource::System)], &s, now);
        db.flush_battery_health();
        assert_eq!(
            counts(&db),
            vec![
                ("2026-10-01".to_string(), 0, 0, 1),
                ("2026-10-01".to_string(), 1, 0, 0),
            ]
        );
    }

    #[test]
    fn a_system_soc_event_without_modules_records_nothing() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        db.record_soc_events(&[soc_jump(SocSource::System)], &snap(vec![]), now);
        db.flush_battery_health();
        assert!(all_rows(&db).is_empty());
    }

    #[test]
    fn soc_event_counts_add_across_flushes_and_restarts() {
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-soc-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = dir.join("h.db");
        let now = at(2026, 10, 1, 9, 0);
        let s = snap(vec![module(0, &[3.3, 3.31], 60)]);
        {
            let db = HistoryDb::open(&path).unwrap();
            db.observe_battery_health(&s, now);
            db.record_soc_events(&[soc_jump(SocSource::Module(0))], &s, now);
            db.flush_battery_health();
            db.record_soc_events(&[soc_jump(SocSource::Module(0))], &s, now);
            db.flush_battery_health();
        }
        let db = HistoryDb::open(&path).unwrap();
        db.record_soc_events(&[soc_stuck(SocSource::Module(0))], &s, now);
        db.flush_battery_health();
        assert_eq!(counts(&db), vec![("2026-10-01".to_string(), 0, 2, 1)]);
    }

    #[test]
    fn soc_events_survive_a_failed_flush() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        let s = snap(vec![module(0, &[3.3, 3.31], 60)]);
        db.observe_battery_health(&s, now);
        db.record_soc_events(&[soc_jump(SocSource::Module(0))], &s, now);
        db.conn
            .lock()
            .unwrap()
            .execute_batch("ALTER TABLE battery_health_daily RENAME TO parked")
            .unwrap();
        db.flush_battery_health();
        db.record_soc_events(&[soc_jump(SocSource::Module(0))], &s, now);
        db.conn
            .lock()
            .unwrap()
            .execute_batch("ALTER TABLE parked RENAME TO battery_health_daily")
            .unwrap();
        db.flush_battery_health();
        assert_eq!(counts(&db), vec![("2026-10-01".to_string(), 0, 2, 0)]);
    }

    #[test]
    fn a_database_from_before_soc_counts_gains_them_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-legacy-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE battery_health_daily (
                    day TEXT NOT NULL, module_index INTEGER NOT NULL,
                    min_cell_v REAL, max_cell_v REAL, max_spread_mv REAL,
                    full_spread_mv REAL, cycles INTEGER, max_temp_c REAL,
                    capacity_ah REAL, design_capacity_ah REAL, bms_firmware INTEGER,
                    samples INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    PRIMARY KEY (day, module_index));
                 INSERT INTO battery_health_daily (day, module_index, max_spread_mv, cycles,
                    samples, updated_at) VALUES ('2026-09-30', 0, 42.0, 300, 1440, 1);",
            )
            .unwrap();
        }
        let db = HistoryDb::open(&path).unwrap();
        let old = &all_rows(&db)[0];
        assert_eq!(old.max_spread_mv, Some(42.0));
        assert_eq!(old.cycles, Some(300));
        assert_eq!((old.soc_jumps, old.soc_stuck), (0, 0));

        let now = at(2026, 10, 1, 12, 0);
        let s = snap(vec![module(0, &[3.3, 3.31], 60)]);
        db.observe_battery_health(&s, now);
        db.record_soc_events(&[soc_jump(SocSource::Module(0))], &s, now);
        db.flush_battery_health();
        assert_eq!(
            counts(&db),
            vec![
                ("2026-09-30".to_string(), 0, 0, 0),
                ("2026-10-01".to_string(), 0, 1, 0),
            ]
        );
    }

    #[test]
    fn a_collapsing_cell_is_recorded() {
        let db = test_db();
        record(
            &db,
            &snap(vec![module(0, &[3.30, 2.10, 3.31], 40)]),
            at(2026, 10, 1, 12, 0),
        );
        assert!((all_rows(&db)[0].max_spread_mv.unwrap() - 1210.0).abs() < 0.5);
    }

    #[test]
    fn module_without_cells_still_records_cycles() {
        let db = test_db();
        record(&db, &snap(vec![module(0, &[], 60)]), at(2026, 10, 1, 12, 0));
        let r = &all_rows(&db)[0];
        assert_eq!(r.max_spread_mv, None);
        assert_eq!(r.cycles, Some(120));
    }

    #[test]
    fn a_corrupt_serial_read_does_not_split_a_modules_row() {
        // The same battery read back with different serials (corrupt reads)
        // is still one module: one row, every poll counted.
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        for serial in ["BAT000132", "BAT00013D", "", "\0\0 ", "BAT00013F"] {
            let mut m = module(0, &[3.30, 3.31], 60);
            m.serial = serial.to_string();
            record(&db, &snap(vec![m]), now);
        }
        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].samples, 5);
    }

    #[test]
    fn zero_cycles_and_capacity_are_not_recorded_as_values() {
        // HV modules decode these as 0: unknown, not zero.
        let db = test_db();
        let mut m = module(0, &[3.40, 3.41], 80);
        m.num_cycles = 0;
        m.capacity_ah = 0.0;
        m.design_capacity_ah = 0.0;
        m.bms_firmware = 0;
        record(&db, &snap(vec![m]), at(2026, 10, 1, 12, 0));
        let r = &all_rows(&db)[0];
        assert_eq!(r.cycles, None);
        assert_eq!(r.capacity_ah, None);
        assert_eq!(r.design_capacity_ah, None);
        assert_eq!(r.bms_firmware, None);
    }

    #[test]
    fn hv_modules_with_the_nominal_capacity_as_both_figures_record_no_capacity() {
        // HV stacks give every module the BCU's nominal Ah as capacity AND
        // design, so capacity/design would read 100% for ever. The HV decoder
        // leaves firmware and cycles at 0; a real BMS reports a firmware.
        let db = test_db();
        let mut hv = module(0, &[3.40, 3.41], 80);
        hv.bms_firmware = 0;
        hv.num_cycles = 0;
        hv.capacity_ah = 100.0;
        hv.design_capacity_ah = 100.0;
        record(&db, &snap(vec![hv]), at(2026, 10, 1, 12, 0));
        let r = &all_rows(&db)[0];
        assert_eq!(r.capacity_ah, None);
        assert_eq!(r.design_capacity_ah, None);
        assert!(r.max_spread_mv.is_some(), "spread still records");
    }

    #[test]
    fn a_corrupt_temperature_is_ignored_and_a_corrupt_cycle_count_cannot_lower_the_max() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        let mut hot = module(0, &[3.30, 3.31], 60);
        hot.temperature = 400.0;
        record(&db, &snap(vec![hot]), now);
        assert_eq!(all_rows(&db)[0].max_temp_c, None);

        let mut fewer = module(0, &[3.30, 3.31], 60);
        fewer.num_cycles = 3;
        record(&db, &snap(vec![fewer]), now);
        assert_eq!(all_rows(&db)[0].cycles, Some(120));
    }

    #[test]
    fn latest_capacity_wins_within_a_day() {
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        record(&db, &snap(vec![module(0, &[3.3, 3.31], 60)]), now);
        let mut later = module(0, &[3.3, 3.31], 60);
        later.capacity_ah = 93.0;
        record(&db, &snap(vec![later]), now);
        assert_eq!(all_rows(&db)[0].capacity_ah.map(|c| c.round()), Some(93.0));
    }

    #[test]
    fn query_is_bounded_by_day() {
        let db = test_db();
        let s = snap(vec![module(0, &[3.30, 3.31], 60)]);
        for d in 1..=5 {
            record(&db, &s, at(2026, 10, d, 12, 0));
        }
        let rows = db
            .query_battery_health(day(2026, 10, 2), day(2026, 10, 4))
            .unwrap();
        let days: Vec<&str> = rows.iter().map(|r| r.day.as_str()).collect();
        assert_eq!(days, vec!["2026-10-02", "2026-10-03", "2026-10-04"]);
    }

    #[test]
    fn a_snapshot_without_modules_writes_nothing_and_asks_for_no_flush() {
        let db = test_db();
        let due = db.observe_battery_health(&snap(vec![]), at(2026, 10, 1, 12, 0));
        assert!(!due);
        db.flush_battery_health();
        assert!(all_rows(&db).is_empty());
    }

    #[test]
    fn flushes_are_throttled_but_a_day_change_and_a_backwards_clock_force_one() {
        let mut acc = HealthAccumulator::default();
        let s = snap(vec![module(0, &[3.30, 3.31], 60)]);

        assert!(acc.observe(&s, at(2026, 10, 1, 12, 0)), "first poll");
        assert!(!acc.observe(&s, at(2026, 10, 1, 12, 1)), "within interval");
        assert!(!acc.observe(&s, at(2026, 10, 1, 12, 4)), "within interval");
        assert!(acc.observe(&s, at(2026, 10, 1, 12, 6)), "interval elapsed");

        assert!(!acc.observe(&s, at(2026, 10, 1, 12, 7)));
        assert!(acc.observe(&s, at(2026, 10, 1, 11, 0)), "clock moved back");

        let _ = acc.take();
        assert!(!acc.observe(&s, at(2026, 10, 1, 11, 1)));
        // Held rows for 1 Oct, then the day changes.
        assert!(acc.observe(&s, at(2026, 10, 2, 0, 0)), "day ended");
    }

    #[test]
    fn rows_held_across_midnight_stay_on_their_own_day() {
        let db = test_db();
        let s = snap(vec![module(0, &[3.30, 3.40], 60)]);
        // Both polls are folded before any flush, as when a flush is late.
        db.observe_battery_health(&s, at(2026, 10, 1, 23, 59));
        let calm = snap(vec![module(0, &[3.30, 3.31], 60)]);
        db.observe_battery_health(&calm, at(2026, 10, 2, 0, 0));
        db.flush_battery_health();

        let rows = all_rows(&db);
        assert_eq!(rows.len(), 2);
        assert!((rows[0].max_spread_mv.unwrap() - 100.0).abs() < 0.1);
        assert!((rows[1].max_spread_mv.unwrap() - 10.0).abs() < 0.1);
    }

    #[test]
    fn concurrent_observers_and_flushers_lose_no_samples() {
        // A poll folding while another flush runs must not drop or double
        // count: every sample ends up in exactly one flush.
        let db = std::sync::Arc::new(test_db());
        let now = at(2026, 10, 1, 12, 0);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let s = snap(vec![module(0, &[3.30, 3.31], 60)]);
                    for _ in 0..50 {
                        db.observe_battery_health(&s, now);
                        db.flush_battery_health();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        db.flush_battery_health();
        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].samples, 200);
    }

    // ---- Batteries that move slot (issue #427) ----

    const GEN2: &str = "BM0000002A";
    const GEN3: &str = "BM0000003B";

    /// A module in `slot` reporting `serial`, its cells `spread_mv` apart.
    fn battery(slot: usize, serial: &str, spread_mv: f32) -> BatteryModule {
        let mut m = module(slot, &[3.300, 3.300 + spread_mv / 1000.0], 60);
        m.serial = serial.to_string();
        m
    }

    /// Enough polls for a slot to adopt its serial.
    fn settle(db: &HistoryDb, modules: Vec<BatteryModule>, now: DateTime<Local>) {
        let s = snap(modules);
        for _ in 0..SERIAL_CONFIRM_READS {
            record(db, &s, now);
        }
    }

    fn spreads(db: &HistoryDb, slot: i64) -> Vec<(String, i64)> {
        all_rows(db)
            .into_iter()
            .filter(|r| r.module_index == slot)
            .map(|r| (r.day, r.max_spread_mv.unwrap().round() as i64))
            .collect()
    }

    /// Gen 2 alone for two days, then a Gen 3 takes the master slot and the
    /// Gen 2 becomes the second battery.
    fn gen3_added_as_master(db: &HistoryDb) {
        for d in [1, 2] {
            settle(db, vec![battery(0, GEN2, 40.0)], at(2026, 10, d, 12, 0));
        }
        for d in [3, 4] {
            settle(
                db,
                vec![battery(0, GEN3, 10.0), battery(1, GEN2, 40.0)],
                at(2026, 10, d, 12, 0),
            );
        }
    }

    #[test]
    fn a_battery_that_moves_slot_takes_its_history_with_it() {
        let db = test_db();
        gen3_added_as_master(&db);

        // The Gen 2's days before the swap are reported under slot 1, where
        // it sits now, and slot 0 holds only the Gen 3.
        assert_eq!(
            spreads(&db, 1),
            vec![
                ("2026-10-01".to_string(), 40),
                ("2026-10-02".to_string(), 40),
                ("2026-10-03".to_string(), 40),
                ("2026-10-04".to_string(), 40),
            ]
        );
        assert_eq!(
            spreads(&db, 0),
            vec![
                ("2026-10-03".to_string(), 10),
                ("2026-10-04".to_string(), 10)
            ]
        );
    }

    #[test]
    fn the_swap_day_does_not_mix_the_two_batteries() {
        let db = test_db();
        settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 9, 30, 12, 0));
        settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 10, 1, 9, 0));
        settle(
            &db,
            vec![battery(0, GEN3, 10.0), battery(1, GEN2, 40.0)],
            at(2026, 10, 1, 15, 0),
        );

        // The Gen 3's afternoon polls make one row on slot 0, and nothing of
        // the Gen 2's 40 mV reaches it.
        let slot0 = spreads(&db, 0);
        assert_eq!(slot0, vec![("2026-10-01".to_string(), 10)]);
        let slot1: Vec<_> = spreads(&db, 1);
        assert_eq!(
            slot1,
            vec![
                ("2026-09-30".to_string(), 40),
                ("2026-10-01".to_string(), 40)
            ]
        );
    }

    #[test]
    fn history_from_before_serials_were_kept_follows_the_first_battery_seen() {
        // Rows written by an earlier release have no serial. They belong to
        // whichever battery then reported a serial in that slot.
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-pre-serial-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE battery_health_daily (
                    day TEXT NOT NULL, module_index INTEGER NOT NULL,
                    min_cell_v REAL, max_cell_v REAL, max_spread_mv REAL,
                    full_spread_mv REAL, cycles INTEGER, max_temp_c REAL,
                    capacity_ah REAL, design_capacity_ah REAL, bms_firmware INTEGER,
                    samples INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    soc_jumps INTEGER NOT NULL DEFAULT 0,
                    soc_stuck INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (day, module_index));
                 INSERT INTO battery_health_daily (day, module_index, max_spread_mv, cycles,
                    samples, updated_at) VALUES ('2026-09-29', 0, 41.0, 300, 1440, 1),
                    ('2026-09-30', 0, 42.0, 301, 1440, 2);",
            )
            .unwrap();
        }
        let db = HistoryDb::open(&path).unwrap();
        gen3_added_as_master(&db);

        let slot1: Vec<_> = spreads(&db, 1).into_iter().map(|(d, _)| d).collect();
        assert_eq!(
            slot1,
            vec![
                "2026-09-29",
                "2026-09-30",
                "2026-10-01",
                "2026-10-02",
                "2026-10-03",
                "2026-10-04"
            ]
        );
        assert_eq!(spreads(&db, 0).len(), 2, "only the Gen 3's own days");
    }

    #[test]
    fn rows_with_no_serial_stay_in_their_slot_when_no_serial_was_ever_seen() {
        let db = test_db();
        record(
            &db,
            &snap(vec![
                module(0, &[3.3, 3.31], 60),
                module(1, &[3.3, 3.32], 60),
            ]),
            at(2026, 10, 1, 12, 0),
        );
        assert_eq!(all_rows(&db).len(), 2);
        assert_eq!(spreads(&db, 1), vec![("2026-10-01".to_string(), 20)]);
    }

    #[test]
    fn a_removed_battery_keeps_its_last_slot() {
        let db = test_db();
        settle(
            &db,
            vec![battery(0, GEN3, 10.0), battery(1, GEN2, 40.0)],
            at(2026, 10, 1, 12, 0),
        );
        settle(&db, vec![battery(0, GEN3, 10.0)], at(2026, 10, 2, 12, 0));
        assert_eq!(spreads(&db, 1), vec![("2026-10-01".to_string(), 40)]);
        assert_eq!(spreads(&db, 0).len(), 2);
    }

    #[test]
    fn a_corrupt_serial_read_cannot_move_a_batterys_history() {
        // Reads that look like serials but disagree, short of a settled run,
        // leave the battery's serial alone: one row, every poll counted.
        let db = test_db();
        let now = at(2026, 10, 1, 12, 0);
        settle(&db, vec![battery(0, GEN2, 40.0)], now);
        for serial in ["BM0000002B", "BM0000002A", "XM0000002A", "BM0000002A"] {
            record(&db, &snap(vec![battery(0, serial, 40.0)]), now);
        }
        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].samples, 5,
            "the two odd reads are left out, not split off"
        );
    }

    #[test]
    fn polls_of_a_new_serial_are_not_filed_under_the_old_battery() {
        // Until a new serial has settled, its polls belong to neither
        // battery: they must not reach the previous one's row (its capacity,
        // firmware, extremes and sample count).
        let db = test_db();
        settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 10, 1, 12, 0));

        let mut gen3 = battery(0, GEN3, 90.0);
        gen3.capacity_ah = 150.0;
        let swapped = snap(vec![gen3, battery(1, GEN2, 40.0)]);
        let now = at(2026, 10, 2, 12, 0);
        for _ in 0..SERIAL_CONFIRM_READS {
            record(&db, &swapped, now);
        }

        let conn = rusqlite::Connection::open(db.path()).unwrap();
        let gen2_in_slot0_today: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM battery_health_daily \
                 WHERE day = '2026-10-02' AND module_index = 0 AND serial = ?1",
                [GEN2],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            gen2_in_slot0_today, 0,
            "the Gen 3's polls reached the Gen 2's row"
        );

        let gen3_today = all_rows(&db)
            .into_iter()
            .find(|r| r.day == "2026-10-02" && r.module_index == 0)
            .unwrap();
        assert_eq!(
            gen3_today.samples, 1,
            "only the poll that settled the serial"
        );
        assert_eq!(gen3_today.capacity_ah.map(f64::round), Some(150.0));
    }

    #[test]
    fn removing_the_master_does_not_blend_it_with_the_battery_that_takes_its_slot() {
        // A and B coexist in slots 0 and 1; A is removed and B becomes the
        // master. Both now claim slot 0, but their days overlap, so merging
        // them would blend two packs into one line: A's history goes to a
        // spare slot instead.
        let db = test_db();
        for d in [1, 2] {
            settle(
                &db,
                vec![battery(0, GEN2, 40.0), battery(1, GEN3, 15.0)],
                at(2026, 10, d, 12, 0),
            );
        }
        for d in [3, 4] {
            settle(&db, vec![battery(0, GEN3, 15.0)], at(2026, 10, d, 12, 0));
        }
        let days = |slot| spreads(&db, slot).into_iter().collect::<Vec<_>>();
        let gen3: Vec<_> = (1..=4).map(|d| (format!("2026-10-0{d}"), 15)).collect();
        assert_eq!(
            days(0),
            gen3,
            "the Gen 3 keeps one line under the master slot"
        );
        assert!(days(1).is_empty());
        assert_eq!(
            days(2),
            vec![
                ("2026-10-01".to_string(), 40),
                ("2026-10-02".to_string(), 40)
            ],
            "the removed Gen 2 keeps its own line"
        );
    }

    #[test]
    fn a_replacement_in_the_same_slot_continues_the_line() {
        // Days that never overlap are one slot's history, as before.
        let db = test_db();
        settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 10, 1, 12, 0));
        settle(&db, vec![battery(0, GEN3, 10.0)], at(2026, 10, 2, 12, 0));
        assert_eq!(spreads(&db, 0).len(), 2);
        assert!(spreads(&db, 1).is_empty());
    }

    #[test]
    fn a_soc_event_while_a_new_serial_settles_is_not_counted_on_the_old_battery() {
        let db = test_db();
        settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 10, 1, 12, 0));
        let now = at(2026, 10, 2, 12, 0);
        let swapped = snap(vec![battery(0, GEN3, 10.0)]);
        db.observe_battery_health(&swapped, now);
        db.record_soc_events(&[soc_jump(SocSource::Module(0))], &swapped, now);
        db.flush_battery_health();
        let jumps: i64 = all_rows(&db).iter().map(|r| r.soc_jumps).sum();
        assert_eq!(
            jumps, 0,
            "an event about the unsettled battery is not the Gen 2's"
        );

        for _ in 1..SERIAL_CONFIRM_READS {
            db.observe_battery_health(&swapped, now);
        }
        db.record_soc_events(&[soc_jump(SocSource::Module(0))], &swapped, now);
        db.flush_battery_health();
        let today = all_rows(&db)
            .into_iter()
            .find(|r| r.day == "2026-10-02")
            .unwrap();
        assert_eq!(today.soc_jumps, 1, "once settled, it counts on the Gen 3");
    }

    #[test]
    fn a_serial_two_modules_share_identifies_neither() {
        let db = test_db();
        settle(
            &db,
            vec![battery(0, GEN2, 40.0), battery(1, GEN2, 15.0)],
            at(2026, 10, 1, 12, 0),
        );
        assert_eq!(spreads(&db, 0), vec![("2026-10-01".to_string(), 40)]);
        assert_eq!(spreads(&db, 1), vec![("2026-10-01".to_string(), 15)]);
    }

    #[test]
    fn a_restart_continues_a_batterys_row_as_one() {
        // A restart expects the battery it last saw, so its first polls join
        // that battery's row rather than waiting for the serial to settle.
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-serial-restart-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = dir.join("h.db");
        let now = at(2026, 10, 1, 12, 0);
        let stored = || -> i64 {
            rusqlite::Connection::open(&path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM battery_health_daily", [], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        {
            let db = HistoryDb::open(&path).unwrap();
            settle(&db, vec![battery(0, GEN2, 40.0)], now);
        }
        let first_run = stored();
        let db = HistoryDb::open(&path).unwrap();
        record(&db, &snap(vec![battery(0, GEN2, 40.0)]), now);
        assert_eq!(
            stored(),
            first_run,
            "the first poll after a restart made no new row"
        );
        let rows = all_rows(&db);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].samples, 4);
    }

    #[test]
    fn soc_events_follow_the_battery_that_moved() {
        let db = test_db();
        gen3_added_as_master(&db);
        let now = at(2026, 10, 5, 12, 0);
        let s = snap(vec![battery(0, GEN3, 10.0), battery(1, GEN2, 40.0)]);
        settle(&db, s.battery_modules.clone(), now);
        db.record_soc_events(&[soc_jump(SocSource::Module(1))], &s, now);
        db.flush_battery_health();
        let rows = all_rows(&db);
        let gen2_today = rows
            .iter()
            .find(|r| r.day == "2026-10-05" && r.module_index == 1)
            .unwrap();
        assert_eq!(gen2_today.soc_jumps, 1);
    }

    #[test]
    fn the_serial_key_migration_is_idempotent_and_keeps_every_row() {
        let dir = std::env::temp_dir().join(format!(
            "givenergy-battery-health-rekey-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = dir.join("h.db");
        {
            let db = HistoryDb::open(&path).unwrap();
            settle(&db, vec![battery(0, GEN2, 40.0)], at(2026, 10, 1, 12, 0));
        }
        let before = {
            let db = HistoryDb::open(&path).unwrap();
            all_rows(&db)
        };
        let again = HistoryDb::open(&path).unwrap();
        assert_eq!(all_rows(&again), before);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'battery_health_daily_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "the rebuild leaves no old table behind");
    }

    #[test]
    fn plausible_serials_are_ten_character_alphanumerics_not_fill() {
        assert_eq!(plausible_serial(" BM0000002A "), Some("BM0000002A"));
        for bad in [
            "",
            "\0\0 ",
            "BAT0",
            "BM00\u{ff}0002A",
            "BM-0000002A",
            "A234567890123456789012",
        ] {
            assert_eq!(plausible_serial(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_slot_changes_serial_only_after_a_settled_run() {
        let mut slot = SlotSerial::default();
        // Nothing settled yet: polls are kept, filed under no serial.
        assert!(!slot.read(Some("BM0000002A")));
        assert!(!slot.read(Some("BM0000002A")));
        assert_eq!(slot.key(), "", "two reads are not settled");
        assert!(!slot.read(Some("BM0000002A")));
        assert_eq!(slot.key(), "BM0000002A");
        // A different serial is left out until it settles; a failed read
        // neither counts towards it nor is left out.
        assert!(slot.read(Some("BM0000003B")));
        assert!(!slot.read(None));
        assert!(slot.read(Some("BM0000003B")));
        assert_eq!(
            slot.key(),
            "BM0000002A",
            "a failed read doesn't count as a new serial"
        );
        assert!(
            !slot.read(Some("BM0000003B")),
            "the poll that settles it is kept"
        );
        assert_eq!(slot.key(), "BM0000003B");
        // A stray read of another serial, then the settled one again.
        assert!(slot.read(Some("BM0000009Z")));
        assert!(!slot.read(Some("BM0000003B")));
    }
}
