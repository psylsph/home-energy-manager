#!/usr/bin/env python3
"""Regression tests for scripts/backfill-history.py."""

import importlib.util
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from datetime import date
from pathlib import Path
from zoneinfo import ZoneInfo


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "backfill-history.py"

spec = importlib.util.spec_from_file_location("backfill_history", SCRIPT)
assert spec and spec.loader
backfill = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = backfill
spec.loader.exec_module(backfill)


READING_COLUMNS = (
    "timestamp",
    "solar_power",
    "pv1_power",
    "pv2_power",
    "battery_power",
    "grid_power",
    "home_power",
    "pv1_voltage",
    "pv2_voltage",
    "pv1_current",
    "pv2_current",
    "soc",
    "battery_voltage",
    "battery_current",
    "battery_temperature",
    "battery_capacity_kwh",
    "grid_voltage",
    "grid_frequency",
    "inverter_temperature",
    "today_solar_kwh",
    "today_pv1_kwh",
    "today_pv2_kwh",
    "today_import_kwh",
    "today_export_kwh",
    "today_charge_kwh",
    "today_discharge_kwh",
    "today_consumption_kwh",
    "today_ac_charge_kwh",
    "home_energy_today_kwh",
    "charge_rate",
    "discharge_rate",
    "battery_reserve",
    "target_soc",
)


def create_history_db(path: Path) -> None:
    columns = ", ".join(
        f"{name} {'INTEGER' if name in {'timestamp', 'soc', 'charge_rate', 'discharge_rate', 'battery_reserve', 'target_soc'} else 'REAL'}"
        for name in READING_COLUMNS
    )
    with sqlite3.connect(path) as conn:
        conn.execute(f"CREATE TABLE readings ({columns}, PRIMARY KEY (timestamp))")


class BackfillHistoryTests(unittest.TestCase):
    def run_script(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_missing_database_is_rejected_without_creating_a_file(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            result = self.run_script("--db", str(path), "--days", "0", "--interval", "3600")

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(path.exists())
            self.assertIn("does not exist", result.stderr)

    def test_incomplete_schema_is_rejected_before_any_insert(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            with sqlite3.connect(path) as conn:
                conn.execute("CREATE TABLE readings (timestamp INTEGER PRIMARY KEY)")

            result = self.run_script("--db", str(path), "--days", "0", "--interval", "3600")

            self.assertNotEqual(result.returncode, 0)
            with sqlite3.connect(path) as conn:
                self.assertEqual(conn.execute("SELECT COUNT(*) FROM readings").fetchone()[0], 0)
            self.assertIn("missing columns", result.stderr)

    def test_default_database_requires_explicit_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            env = {**os.environ, "HOME": temp_dir}
            result = self.run_script("--days", "0", "--interval", "3600", env=env)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--allow-live-db", result.stderr)
            self.assertFalse((Path(temp_dir) / ".givenergy-local" / "history.db").exists())

    def test_valid_schema_accepts_the_full_reading_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)

            result = self.run_script("--db", str(path), "--days", "0", "--interval", "86400")

            self.assertEqual(result.returncode, 0, result.stderr)
            with sqlite3.connect(path) as conn:
                row = conn.execute(
                    "SELECT COUNT(*), today_pv1_kwh, today_pv2_kwh FROM readings"
                ).fetchone()
            self.assertEqual(row[0], 1)
            self.assertIsNotNone(row[1])
            self.assertIsNotNone(row[2])

    def test_local_dst_days_have_23_or_25_hours(self) -> None:
        tz = ZoneInfo("Europe/London")
        spring = backfill.generate_day(date(2026, 3, 29), 3600, {"soc": 45.0}, tz)
        autumn = backfill.generate_day(date(2026, 10, 25), 3600, {"soc": 45.0}, tz)

        self.assertEqual(len(spring), 23)
        self.assertEqual(len(autumn), 25)
        self.assertEqual(spring[-1][0] - spring[0][0], 22 * 3600)
        self.assertEqual(autumn[-1][0] - autumn[0][0], 24 * 3600)

    def test_generation_is_deterministic_and_resume_preserves_prefix(self) -> None:
        tz = ZoneInfo("Europe/London")
        first = backfill.generate_day(date(2026, 6, 15), 900, {"soc": 45.0}, tz)
        resumed = backfill.generate_day(date(2026, 6, 15), 900, {"soc": 45.0}, tz)

        self.assertEqual(resumed, first)
        prefix_length = len(first) // 2
        self.assertEqual(resumed[:prefix_length], first[:prefix_length])

        for index in range(20, 29):
            counters = [row[index] for row in resumed]
            self.assertTrue(
                all(left <= right for left, right in zip(counters, counters[1:])),
                READING_COLUMNS[index],
            )

    def test_pv_daily_counters_are_written_for_each_string(self) -> None:
        tz = ZoneInfo("Europe/London")
        rows = backfill.generate_day(date(2026, 6, 15), 900, {"soc": 45.0}, tz)

        pv1_counter = [row[20] for row in rows]
        pv2_counter = [row[21] for row in rows]
        solar_counter = [row[19] for row in rows]
        self.assertGreater(pv1_counter[-1], 0)
        self.assertEqual(pv2_counter[-1], 0)
        self.assertEqual(pv1_counter[-1], solar_counter[-1])


    # -- daily battery health (issue #393) ---------------------------------

    def health_rows(self, path: Path) -> list[sqlite3.Row]:
        with sqlite3.connect(path) as conn:
            conn.row_factory = sqlite3.Row
            return conn.execute(
                "SELECT * FROM battery_health_daily ORDER BY day, module_index"
            ).fetchall()

    def test_battery_health_table_matches_the_apps_schema(self) -> None:
        rust = (ROOT / "src-tauri" / "src" / "history" / "mod.rs").read_text()
        block = rust.split("CREATE TABLE IF NOT EXISTS battery_health_daily (")[1].split(");")[0]
        rust_columns = [
            line.split()[0] for line in block.splitlines()
            if line.strip() and not line.strip().startswith(("--", "PRIMARY"))
        ]
        self.assertEqual(rust_columns, list(backfill.BATTERY_HEALTH_COLUMNS))
        with sqlite3.connect(":memory:") as conn:
            conn.execute(backfill.BATTERY_HEALTH_DDL)
            script_columns = [row[1] for row in conn.execute("PRAGMA table_info(battery_health_daily)")]
        self.assertEqual(script_columns, rust_columns)

    def test_backfill_writes_one_health_row_per_module_per_day(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)

            result = self.run_script("--db", str(path), "--days", "9", "--interval", "3600")

            self.assertEqual(result.returncode, 0, result.stderr)
            rows = self.health_rows(path)
            days = {row["day"] for row in rows}
            self.assertEqual(len(rows), 2 * len(days))
            self.assertEqual({row["module_index"] for row in rows}, {0, 1})
            for row in rows:
                self.assertLess(row["min_cell_v"], row["max_cell_v"])
                self.assertGreaterEqual(row["max_spread_mv"], row["full_spread_mv"] or 0)
                self.assertEqual(row["design_capacity_ah"], backfill.HEALTH_DESIGN_AH)
                self.assertLess(row["capacity_ah"], row["design_capacity_ah"])
                self.assertGreater(row["samples"], 0)
            self.assertIn("Battery health:", result.stdout)

    def test_health_rerun_adds_nothing_and_keeps_existing_rows(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)
            args = ("--db", str(path), "--days", "5", "--interval", "3600")
            self.assertEqual(self.run_script(*args).returncode, 0)
            first = [tuple(row) for row in self.health_rows(path)]
            with sqlite3.connect(path) as conn:
                conn.execute("UPDATE battery_health_daily SET cycles = 999999 WHERE module_index = 0")

            self.assertEqual(self.run_script(*args).returncode, 0)

            rows = self.health_rows(path)
            self.assertEqual(len(rows), len(first))
            self.assertTrue(all(row["cycles"] == 999999 for row in rows if row["module_index"] == 0))

    def test_only_the_weakening_module_has_soc_warnings(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)

            result = self.run_script("--db", str(path), "--days", "120", "--interval", "3600")

            self.assertEqual(result.returncode, 0, result.stderr)
            rows = self.health_rows(path)
            healthy = [row for row in rows if row["module_index"] == 0]
            weak = [row for row in rows if row["module_index"] == 1]
            self.assertTrue(all(row["soc_jumps"] == 0 and row["soc_stuck"] == 0 for row in healthy))
            self.assertGreater(sum(row["soc_jumps"] for row in weak), 0)
            self.assertGreater(sum(row["soc_stuck"] for row in weak), 0)

    def test_an_older_health_table_gains_the_soc_columns_and_keeps_its_rows(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)
            old_ddl = backfill.BATTERY_HEALTH_DDL.replace(
                "    soc_jumps          INTEGER NOT NULL DEFAULT 0,\n", ""
            ).replace("    soc_stuck          INTEGER NOT NULL DEFAULT 0,\n", "")
            self.assertNotIn("soc_jumps", old_ddl)
            with sqlite3.connect(path) as conn:
                conn.execute(old_ddl)
                conn.execute(
                    "INSERT INTO battery_health_daily (day, module_index, samples, updated_at) "
                    "VALUES ('2000-01-01', 0, 1, 1)"
                )

            result = self.run_script("--db", str(path), "--days", "2", "--interval", "3600")

            self.assertEqual(result.returncode, 0, result.stderr)
            rows = self.health_rows(path)
            self.assertEqual(rows[0]["day"], "2000-01-01")
            self.assertEqual(rows[0]["soc_jumps"], 0)
            self.assertGreater(len(rows), 1)

    def test_modules_zero_skips_battery_health_entirely(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)

            result = self.run_script("--db", str(path), "--days", "2", "--interval", "3600", "--modules", "0")

            self.assertEqual(result.returncode, 0, result.stderr)
            with sqlite3.connect(path) as conn:
                table = conn.execute(
                    "SELECT 1 FROM sqlite_master WHERE name = 'battery_health_daily'"
                ).fetchone()
            self.assertIsNone(table)

    def test_negative_modules_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "history.db"
            create_history_db(path)
            result = self.run_script("--db", str(path), "--modules", "-1")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--modules", result.stderr)

    def test_weak_module_spread_widens_while_the_healthy_one_stays_flat(self) -> None:
        tz = ZoneInfo("Europe/London")

        def spread(day: date, module: int) -> float:
            rows = backfill.generate_day(day, 3600, {"soc": 100.0}, tz)
            health = backfill.generate_battery_health(day, rows, 2, 3600)
            return health[module][4]  # max_spread_mv

        early, late = date(2026, 8, 5), date(2026, 12, 5)
        self.assertGreater(spread(late, 1), spread(early, 1) + 30)
        self.assertLess(abs(spread(late, 0) - spread(early, 0)), 15)

    def test_near_full_spread_is_absent_on_days_the_battery_never_filled(self) -> None:
        day = date(2026, 10, 1)
        low = [(1_000_000 + i,) + (0,) * 10 + (60,) + (0, 0, 20.0) for i in range(3)]
        full = [(1_000_000 + i,) + (0,) * 10 + (97,) + (0, 0, 20.0) for i in range(3)]
        self.assertIsNone(backfill.generate_battery_health(day, low, 1, 60)[0][5])
        self.assertIsNotNone(backfill.generate_battery_health(day, full, 1, 60)[0][5])

    def test_health_is_deterministic_and_independent_of_the_days_range(self) -> None:
        tz = ZoneInfo("Europe/London")
        rows = backfill.generate_day(date(2026, 10, 1), 3600, {"soc": 45.0}, tz)
        self.assertEqual(
            backfill.generate_battery_health(date(2026, 10, 1), rows, 2, 3600),
            backfill.generate_battery_health(date(2026, 10, 1), rows, 2, 3600),
        )

    def test_no_rows_or_no_modules_generate_no_health(self) -> None:
        tz = ZoneInfo("Europe/London")
        rows = backfill.generate_day(date(2026, 10, 1), 3600, {"soc": 45.0}, tz)
        self.assertEqual(backfill.generate_battery_health(date(2026, 10, 1), [], 2, 3600), [])
        self.assertEqual(backfill.generate_battery_health(date(2026, 10, 1), rows, 0, 3600), [])


if __name__ == "__main__":
    unittest.main()
