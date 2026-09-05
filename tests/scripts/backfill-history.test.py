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


if __name__ == "__main__":
    unittest.main()
