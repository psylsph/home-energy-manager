#!/usr/bin/env python3
"""
Backfill the HEM history database with realistic synthetic solar/battery data.

Generates physically-consistent readings for a UK residential solar+battery system:
  - 4 kWp solar array, 13.5 kWh LV battery, 240V grid
  - Solar irradiance model based on latitude, day-of-year, cloud cover
  - Battery charge/discharge logic with SOC tracking
  - Cumulative daily counters that reset at midnight
  - Sign convention: home = solar + battery - grid (grid+ = export, bat+ = discharge)

Usage:
  python3 backfill-history.py [--db PATH] [--days N] [--interval SECS]
                              [--timezone NAME] [--allow-live-db]

The default database is protected by an explicit --allow-live-db opt-in;
passing --db points the script at an isolated database explicitly.
"""

import argparse
import math
import random
import sqlite3
from datetime import datetime, time as datetime_time, timezone, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

# ── Physical constants / system parameters ──────────────────────────────────

LATITUDE_RAD = math.radians(51.5)   # UK latitude
DEFAULT_TIMEZONE_NAME = 'Europe/London'
MAX_SOLAR_W = 4000                  # 4 kWp array peak
BATTERY_CAPACITY_KWH = 13.5
BATTERY_CAPACITY_WH = BATTERY_CAPACITY_KWH * 1000
MAX_CHARGE_W = 2600                 # charge rate limit
MAX_DISCHARGE_W = 2600              # discharge rate limit
BATTERY_RESERVE_SOC = 4             # 4% reserve
TARGET_SOC = 100
NOMINAL_BAT_V = 51.0                # LV battery nominal voltage
GRID_VOLTAGE = 240.0
GRID_FREQ = 50.0
BATTERY_CAPACITY_DB = 13.5          # stored in battery_capacity_kwh column

READING_COLUMNS = (
    'timestamp',
    'solar_power',
    'pv1_power',
    'pv2_power',
    'battery_power',
    'grid_power',
    'home_power',
    'pv1_voltage',
    'pv2_voltage',
    'pv1_current',
    'pv2_current',
    'soc',
    'battery_voltage',
    'battery_current',
    'battery_temperature',
    'battery_capacity_kwh',
    'grid_voltage',
    'grid_frequency',
    'inverter_temperature',
    'today_solar_kwh',
    'today_pv1_kwh',
    'today_pv2_kwh',
    'today_import_kwh',
    'today_export_kwh',
    'today_charge_kwh',
    'today_discharge_kwh',
    'today_consumption_kwh',
    'today_ac_charge_kwh',
    'home_energy_today_kwh',
    'charge_rate',
    'discharge_rate',
    'battery_reserve',
    'target_soc',
)


def default_db_path():
    """Return the current user's portable HEM history path."""
    return Path.home() / '.givenergy-local' / 'history.db'


def local_day_bounds(date_obj, local_tz):
    """Return UTC timestamps for the start and end of a local calendar day."""
    start = datetime.combine(date_obj, datetime_time.min, tzinfo=local_tz)
    end = datetime.combine(date_obj + timedelta(days=1), datetime_time.min, tzinfo=local_tz)
    return int(start.timestamp()), int(end.timestamp())


def validate_database(path):
    """Validate an existing HEM database without creating or changing it."""
    if not path.exists():
        raise ValueError(f'database does not exist: {path}')
    if not path.is_file():
        raise ValueError(f'database path is not a file: {path}')

    # mode=ro is important: sqlite3.connect(path) creates a new file when a
    # typo or stale path is supplied, which is especially unhelpful for a
    # backfill utility.
    readonly_uri = f'file:{path.resolve()}?mode=ro'
    try:
        with sqlite3.connect(readonly_uri, uri=True) as conn:
            table = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'readings'"
            ).fetchone()
            if table is None:
                raise ValueError("database is missing the 'readings' table")

            columns = {
                row[1] for row in conn.execute('PRAGMA table_info(readings)')
            }
            missing = sorted(set(READING_COLUMNS) - columns)
            if missing:
                raise ValueError(f'missing columns in readings: {", ".join(missing)}')
    except sqlite3.Error as exc:
        raise ValueError(f'invalid SQLite database: {exc}') from exc


def solar_power_model(hour_utc, day_of_year, cloud_factor,
                      timezone_offset_hours=0, rng=None):
    """
    Estimate instantaneous solar power (W) based on time of day and season.

    Uses a simplified clear-sky model with atmospheric losses.
    cloud_factor: 0.0 (clear) to 1.0 (heavy overcast), reduces output.
    """
    # Solar noon in UTC, accounting for the offset that applies at this
    # instant rather than assuming BST throughout the year.
    solar_noon_utc = 12.0 - timezone_offset_hours

    # Day length varies by season (simplified declination model)
    decl = 23.45 * math.sin(math.radians(360 * (284 + day_of_year) / 365))
    decl_rad = math.radians(decl)
    # Hour angle at sunrise/sunset
    cos_omega = -math.tan(LATITUDE_RAD) * math.tan(decl_rad)
    cos_omega = max(-1, min(1, cos_omega))
    half_day = math.degrees(math.acos(cos_omega)) / 15.0  # hours

    sunrise = solar_noon_utc - half_day
    sunset = solar_noon_utc + half_day

    if hour_utc < sunrise or hour_utc > sunset:
        return 0.0

    # Sinusoidal irradiance: 0 at sunrise/sunset, peak at solar noon
    day_frac = (hour_utc - sunrise) / (sunset - sunrise)
    if day_frac < 0 or day_frac > 1:
        return 0.0

    irradiance_frac = math.sin(day_frac * math.pi)

    # Seasonal peak: max output depends on declination (higher in summer)
    seasonal_peak = MAX_SOLAR_W * (0.55 + 0.45 * math.sin(math.radians((day_of_year - 80) * 360 / 365)))
    seasonal_peak = min(seasonal_peak, MAX_SOLAR_W)

    # Cloud attenuation: heavy clouds reduce to 10-40%, light clouds to 50-80%
    cloud_atten = 1.0 - cloud_factor * 0.75

    # Add small noise
    random_source = random if rng is None else rng
    noise = 1.0 + random_source.gauss(0, 0.03)

    return max(0.0, seasonal_peak * irradiance_frac * cloud_atten * noise)


def home_load_model(hour_local, rng=None):
    """
    Model typical UK home electricity consumption (W).
    Two peaks: morning (07:00-09:00) and evening (17:00-22:00).
    Baseline ~200-400W (standby, fridge, router, etc).
    """
    random_source = random if rng is None else rng
    baseline = random_source.uniform(180, 350)

    # Morning peak (kettle, shower, etc)
    if 6.5 <= hour_local <= 9.5:
        morning_boost = 400 * math.exp(-((hour_local - 7.5) ** 2) / 1.5)
        baseline += morning_boost * random_source.uniform(0.5, 1.5)

    # Evening peak (cooking, TV, lights)
    if 16.0 <= hour_local <= 22.0:
        evening_boost = 800 * math.exp(-((hour_local - 19.0) ** 2) / 4.0)
        baseline += evening_boost * random_source.uniform(0.4, 1.3)

    # Occasional random appliance spikes (washing machine, dishwasher, etc)
    if random_source.random() < 0.02:
        baseline += random_source.uniform(1000, 2500)

    return baseline


def generate_day(date_obj, interval_secs, prev_end_of_day_state, local_tz=None):
    """
    Generate all readings for a single day.

    prev_end_of_day_state: dict with 'soc' (battery SOC at start of day, %)
    Returns list of row tuples.
    """
    if interval_secs <= 0:
        raise ValueError('interval must be positive')
    if local_tz is None:
        local_tz = ZoneInfo(DEFAULT_TIMEZONE_NAME)

    day_of_year = date_obj.timetuple().tm_yday

    # A date-derived generator means a rerun reproduces the existing prefix
    # exactly and gives the same cumulative counters to any newly appended
    # rows. It also keeps tests independent of process-global random state.
    rng = random.Random(date_obj.toordinal())
    base_cloud = max(0.0, min(0.85, 0.3 + rng.gauss(0, 0.2)))

    rows = []
    soc = prev_end_of_day_state['soc']
    bat_voltage = 48.0 + soc * 0.055  # approx 48-53.5V range

    # Cumulative counters (reset at midnight)
    today_solar_kwh = 0.0
    today_pv1_kwh = 0.0
    today_pv2_kwh = 0.0
    today_import_kwh = 0.0   # energy from grid (positive)
    today_export_kwh = 0.0   # energy to grid (positive)
    today_charge_kwh = 0.0
    today_discharge_kwh = 0.0
    today_consumption_kwh = 0.0

    start_ts, end_ts = local_day_bounds(date_obj, local_tz)
    for offset in range(0, end_ts - start_ts, interval_secs):
        ts = start_ts + offset
        step_secs = min(interval_secs, end_ts - ts)
        dt_hours = step_secs / 3600.0
        utc_dt = datetime.fromtimestamp(ts, timezone.utc)
        local_dt = utc_dt.astimezone(local_tz)
        hour_utc = utc_dt.hour + utc_dt.minute / 60.0 + utc_dt.second / 3600.0
        hour_local = local_dt.hour + local_dt.minute / 60.0 + local_dt.second / 3600.0
        timezone_offset = local_dt.utcoffset()
        timezone_offset_hours = (
            timezone_offset.total_seconds() / 3600.0 if timezone_offset is not None else 0
        )

        # Solar with per-step cloud variation
        step_cloud = max(0.0, min(0.95, base_cloud + rng.gauss(0, 0.12)))
        solar = solar_power_model(
            hour_utc, day_of_year, step_cloud, timezone_offset_hours, rng
        )
        solar = round(solar)

        # Home demand
        home = home_load_model(hour_local, rng)

        # Battery logic
        battery_power = 0.0
        excess = solar - home

        available_headroom_wh = (100 - soc) / 100.0 * BATTERY_CAPACITY_WH
        available_energy_wh = (soc - BATTERY_RESERVE_SOC) / 100.0 * BATTERY_CAPACITY_WH

        if excess > 50 and soc < 99.5:
            # Charge
            charge_w = min(excess, MAX_CHARGE_W, available_headroom_wh / dt_hours if dt_hours > 0 else MAX_CHARGE_W)
            charge_w = max(0, charge_w)
            battery_power = -charge_w
            soc += charge_w * dt_hours / BATTERY_CAPACITY_KWH
            soc = min(100.0, soc)
            today_charge_kwh += charge_w * dt_hours / 1000.0
        elif excess < -50 and soc > BATTERY_RESERVE_SOC + 1:
            # Discharge
            discharge_w = min(-excess, MAX_DISCHARGE_W, available_energy_wh / dt_hours if dt_hours > 0 else MAX_DISCHARGE_W)
            discharge_w = max(0, discharge_w)
            battery_power = discharge_w
            soc -= discharge_w * dt_hours / BATTERY_CAPACITY_KWH
            soc = max(float(BATTERY_RESERVE_SOC), soc)
            today_discharge_kwh += discharge_w * dt_hours / 1000.0

        soc_int = int(round(soc))

        # Grid power: home = solar + battery - grid => grid = solar + battery - home
        # grid positive = export, grid negative = import
        grid = solar + battery_power - home

        # Track cumulative counters
        today_solar_kwh += solar * dt_hours / 1000.0
        today_consumption_kwh += home * dt_hours / 1000.0
        if grid > 0:
            today_export_kwh += grid * dt_hours / 1000.0
        else:
            today_import_kwh += (-grid) * dt_hours / 1000.0

        # Battery voltage from SOC (LiFePO4 discharge curve)
        bat_voltage = 49.0 + soc * 0.045  # ~49V at 0%, ~53.5V at 100%
        bat_voltage += rng.gauss(0, 0.15)

        # Battery current: power / voltage, negative = charging
        bat_current = abs(battery_power) / bat_voltage if bat_voltage > 1 else 0
        if battery_power < 0:
            bat_current = -bat_current
        bat_current += rng.gauss(0, 0.1)

        # PV details (single string model: pv1 active, pv2 = 0)
        is_daytime = solar > 5
        if is_daytime:
            pv1_power = solar
            pv2_power = 0
            # PV voltage: higher voltage when producing, ~350V typical for a string
            pv1_voltage = 320 + rng.uniform(0, 40)
            pv2_voltage = 0.0
            pv1_current = solar / pv1_voltage if pv1_voltage > 1 else 0
            pv2_current = 0.0
        else:
            pv1_power = 0
            pv2_power = 0
            pv1_voltage = None  # NULL at night (matches real data)
            pv2_voltage = None
            pv1_current = None
            pv2_current = None

        # Grid voltage and frequency (NULL at night when inverter is off)
        if is_daytime or abs(grid) > 10:
            grid_v = GRID_VOLTAGE + rng.gauss(0, 2)
            grid_f = GRID_FREQ + rng.gauss(0, 0.05)
        else:
            grid_v = None
            grid_f = None

        # Inverter temp: warmer when operating
        if is_daytime:
            inv_temp = 30 + (solar / MAX_SOLAR_W) * 8 + rng.gauss(0, 1)
        else:
            inv_temp = 25 + rng.gauss(0, 1)

        # Battery temp: slowly tracks ambient with charging heating
        bat_temp_base = 20 + (soc / 100) * 3
        if battery_power < -500:
            bat_temp_base += 2  # charging heats battery
        bat_temp = bat_temp_base + rng.gauss(0, 0.5)

        # PV daily counters are separate from the aggregate solar counter.
        today_pv1_kwh += pv1_power * dt_hours / 1000.0
        today_pv2_kwh += pv2_power * dt_hours / 1000.0

        # Determine home_energy_today (cumulative home energy)
        home_energy_today = today_consumption_kwh

        # Round power fields to integers
        solar_r = int(solar) if solar > 0 else 0
        battery_r = int(round(battery_power))
        grid_r = int(round(grid))
        home_r = int(round(home))

        row = (
            ts,
            solar_r,                      # solar_power
            int(pv1_power) if pv1_power else 0,   # pv1_power
            0,                            # pv2_power (single string)
            battery_r,                    # battery_power
            grid_r,                       # grid_power
            home_r,                       # home_power
            round(pv1_voltage, 1) if pv1_voltage is not None else None,
            None,                         # pv2_voltage
            round(pv1_current, 2) if pv1_current is not None else None,
            None,                         # pv2_current
            soc_int,                      # soc
            round(bat_voltage, 2),        # battery_voltage
            round(bat_current, 3),        # battery_current
            round(bat_temp, 1),          # battery_temperature
            BATTERY_CAPACITY_DB,          # battery_capacity_kwh
            round(grid_v, 1) if grid_v is not None else None,
            round(grid_f, 2) if grid_f is not None else None,
            round(inv_temp, 1),          # inverter_temperature
            round(today_solar_kwh, 2),   # today_solar_kwh
            round(today_pv1_kwh, 2),     # today_pv1_kwh
            round(today_pv2_kwh, 2),     # today_pv2_kwh
            round(today_import_kwh, 2),  # today_import_kwh
            round(today_export_kwh, 2),  # today_export_kwh
            round(today_charge_kwh, 2),  # today_charge_kwh
            round(today_discharge_kwh, 2),  # today_discharge_kwh
            round(today_consumption_kwh, 2),  # today_consumption_kwh
            0.0,                          # today_ac_charge_kwh
            round(home_energy_today, 2),  # home_energy_today_kwh
            100,                          # charge_rate (0-100% scale)
            100,                          # discharge_rate
            BATTERY_RESERVE_SOC,          # battery_reserve
            TARGET_SOC,                   # target_soc
        )
        rows.append(row)

    # Return end-of-day SOC for next day
    prev_end_of_day_state['soc'] = soc
    return rows


def main():
    parser = argparse.ArgumentParser(description='Backfill history DB with realistic test data')
    parser.add_argument('--db',
                        help='Path to history.db')
    parser.add_argument('--days', type=int, default=60,
                        help='Number of days of history to generate')
    parser.add_argument('--interval', type=int, default=15,
                        help='Interval between readings in seconds')
    parser.add_argument('--timezone', default=DEFAULT_TIMEZONE_NAME,
                        help=f'IANA timezone for local calendar days (default: {DEFAULT_TIMEZONE_NAME})')
    parser.add_argument('--allow-live-db', action='store_true',
                        help='Allow the portable default database to be modified')
    args = parser.parse_args()

    try:
        local_tz = ZoneInfo(args.timezone)
    except ZoneInfoNotFoundError:
        parser.error(f'unknown IANA timezone: {args.timezone}')
    if args.days < 0:
        parser.error('--days must not be negative')
    if args.interval <= 0:
        parser.error('--interval must be positive')

    using_live_default = args.db is None
    db_path = default_db_path() if using_live_default else Path(args.db).expanduser()
    if using_live_default and not args.allow_live_db:
        parser.error(
            f'refusing to use the live default database {db_path}; '
            'pass --allow-live-db or provide --db PATH'
        )
    try:
        validate_database(db_path)
    except ValueError as exc:
        parser.error(str(exc))

    now = datetime.now(timezone.utc)
    today = now.astimezone(local_tz).date()

    # Start date: N days ago
    start_date = today - timedelta(days=args.days)

    # End: fill up to current time today
    end_ts = int(now.timestamp())

    print(f"Backfilling {args.days} days of data ({start_date} to {today})")
    print(f"  DB: {db_path}")
    print(f"  Timezone: {args.timezone}")
    print(f"  Interval: {args.interval}s")
    estimated_rows = 0
    estimate_date = start_date
    while estimate_date <= today:
        day_start, day_end = local_day_bounds(estimate_date, local_tz)
        estimated_rows += (day_end - day_start + args.interval - 1) // args.interval
        estimate_date += timedelta(days=1)
    print(f"  Est. rows: ~{estimated_rows:,}")

    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    # Start with a reasonable overnight SOC
    state = {'soc': 45.0}

    total_inserted = 0
    total_skipped = 0

    current = start_date
    while current <= today:
        # Generate full day of data
        day_rows = generate_day(current, args.interval, state, local_tz)

        # For today, only insert up to current time
        if current == today:
            day_rows = [r for r in day_rows if r[0] <= end_ts]

        if not day_rows:
            current += timedelta(days=1)
            continue

        # Batch insert with INSERT OR IGNORE (don't overwrite existing rows)
        placeholders = ','.join(['?'] * len(READING_COLUMNS))
        columns = ', '.join(READING_COLUMNS)
        sql = f"INSERT OR IGNORE INTO readings ({columns}) VALUES ({placeholders})"

        cur.executemany(sql, day_rows)
        total_inserted += cur.rowcount
        total_skipped += len(day_rows) - cur.rowcount

        conn.commit()

        date_str = current.strftime('%Y-%m-%d')
        print(f"  {date_str}: {len(day_rows)} rows generated, {cur.rowcount} inserted, "
              f"{len(day_rows) - cur.rowcount} skipped (existing)")

        current += timedelta(days=1)

    conn.commit()

    # Print summary statistics
    cur.execute("SELECT COUNT(*) FROM readings")
    total = cur.fetchone()[0]
    cur.execute("SELECT MIN(timestamp), MAX(timestamp) FROM readings")
    min_ts, max_ts = cur.fetchone()
    min_text = datetime.fromtimestamp(min_ts, local_tz).isoformat() if min_ts is not None else '—'
    max_text = datetime.fromtimestamp(max_ts, local_tz).isoformat() if max_ts is not None else '—'

    print(f"\n{'='*60}")
    print(f"Done! Inserted {total_inserted:,} new rows, skipped {total_skipped:,} existing")
    print(f"Total rows in DB: {total:,}")
    print(f"Date range: {min_text} to {max_text}")

    conn.close()


if __name__ == '__main__':
    main()
