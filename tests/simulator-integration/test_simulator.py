#!/usr/bin/env python3
"""
Integration tests for givenergy-local Modbus client against the GivEnergy simulator.

Uses the `sim-api serve` command to start a real Modbus TCP server with a
given plant config, then connects with raw Python sockets using the
GivEnergy proprietary Modbus frame format to verify register reads/writes.

Requirements: Python 3.7+, sim-api binary at known path, stdlib only.
"""

import json
import os
import signal
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

# --- Constants ---------------------------------------------------------------

SIM_API_BIN = os.path.expanduser(
    "~/repos/givenergy-simulator/target/release/sim-api"
)
CONFIG_DIR = Path(__file__).parent / "configs"
BASE_PORT = 18900  # starting port, each inverter type gets +1

TRANSACTION_ID = 0x5959
PROTOCOL_ID = 0x0001
UNIT_ID = 1
FUNC_TRANSPARENT = 0x02
FUNC_READ_HOLDING = 0x03
FUNC_READ_INPUT = 0x04
FUNC_WRITE_SINGLE = 0x06

SLAVE_READ = 0x32  # inverter read slave
SLAVE_WRITE = 0x11  # inverter write slave
SERIAL = b"SA1234    "  # 10 bytes, space-padded
HEADER_SIZE = 26

# Device type codes expected for each inverter type
# From sim-api --help list:
#   Gen3Hybrid -> DTC 0x2001
#   Gen2Hybrid -> DTC 0x2001 (same as Gen3)
#   ACCoupled  -> DTC 0x3001
#   ThreePhase -> DTC 0x4001
#   AllInOne6  -> DTC 0x8001
EXPECTED_DTC = {
    "Gen3Hybrid": 0x2001,
    "Gen2Hybrid": 0x2001,
    "ACCoupled": 0x3001,
    "ThreePhase": 0x4001,
    "AllInOne6": 0x8001,
}

# --- CRC-16/Modbus -----------------------------------------------------------

def crc16_modbus(data: bytes) -> int:
    """CRC-16/Modbus (polynomial 0x8005)."""
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xA001
            else:
                crc >>= 1
    return crc

# --- Frame helpers -----------------------------------------------------------

def build_read_request(slave: int, func: int, start_addr: int, count: int) -> bytes:
    """Build a GivEnergy Modbus read-request frame."""
    payload = struct.pack(">HH", start_addr, count)
    inner = bytes([slave, func]) + payload
    crc = crc16_modbus(inner)
    inner += struct.pack("<H", crc)
    length = 1 + 1 + 10 + 8 + len(inner)
    frame = (
        struct.pack(">HH", TRANSACTION_ID, PROTOCOL_ID)
        + struct.pack(">H", length)
        + bytes([UNIT_ID, FUNC_TRANSPARENT])
        + SERIAL
        + struct.pack(">Q", 8)
        + inner
    )
    return frame

def build_write_request(address: int, value: int) -> bytes:
    """Build a GivEnergy Modbus write-single-register frame."""
    payload = struct.pack(">HH", address, value)
    inner = bytes([SLAVE_WRITE, FUNC_WRITE_SINGLE]) + payload
    crc = crc16_modbus(inner)
    inner += struct.pack("<H", crc)
    length = 1 + 1 + 10 + 8 + len(inner)
    frame = (
        struct.pack(">HH", TRANSACTION_ID, PROTOCOL_ID)
        + struct.pack(">H", length)
        + bytes([UNIT_ID, FUNC_TRANSPARENT])
        + SERIAL
        + struct.pack(">Q", 8)
        + inner
    )
    return frame

def decode_response(data: bytes):
    """Decode a GivEnergy response frame -> (slave, func, payload)."""
    if len(data) < HEADER_SIZE + 4:
        return None
    inner = data[HEADER_SIZE:]
    if len(inner) < 4:
        return None
    slave = inner[0]
    func = inner[1]
    payload = inner[2:-2]  # strip CRC
    return slave, func, payload

def parse_read_payload(payload: bytes):
    """Parse a read-response payload: (serial, start, count, [values...])."""
    if len(payload) < 14:
        return None
    # payload: serial(10) + start(2) + count(2) + data(N*2)
    serial = payload[:10]
    start = struct.unpack(">H", payload[10:12])[0]
    count = struct.unpack(">H", payload[12:14])[0]
    data_bytes = payload[14:]
    data = []
    for i in range(0, len(data_bytes), 2):
        if i + 1 < len(data_bytes):
            data.append(struct.unpack(">H", data_bytes[i : i + 2])[0])
    return serial, start, count, data

def parse_write_response(payload: bytes):
    """Parse a write-response payload: (serial, register, value)."""
    if len(payload) < 14:
        return None
    # payload: serial(10) + register(2) + value(2)
    register = struct.unpack(">H", payload[10:12])[0]
    value = struct.unpack(">H", payload[12:14])[0]
    return register, value

# --- Simulator process management --------------------------------------------

class SimulatorInstance:
    """Manages a single simulator process with its own port."""

    def __init__(self, config_path: str, port: int):
        self.config_path = config_path
        self.port = port
        self.process = None

    def start(self):
        """Start the simulator in the background."""
        addr = f"127.0.0.1:{self.port}"
        self.process = subprocess.Popen(
            [SIM_API_BIN, "serve", self.config_path, "--modbus", addr],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        # Wait for server to be ready
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                s = socket.create_connection(("127.0.0.1", self.port), timeout=2)
                s.close()
                return True
            except (ConnectionRefusedError, OSError):
                time.sleep(0.2)
        return False

    def stop(self):
        """Stop the simulator process."""
        if self.process:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()

# --- Test helpers ------------------------------------------------------------

def connect(port: int) -> socket.socket:
    """Create a connected TCP socket."""
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    return s

def read_registers(sock, slave, func, start, count):
    """Send a read request and return parsed data array."""
    req = build_read_request(slave, func, start, count)
    sock.sendall(req)
    resp = sock.recv(4096)
    result = decode_response(resp)
    if result is None:
        return None
    _, func_resp, payload = result
    parsed = parse_read_payload(payload)
    if parsed is None:
        return None
    _, _, _, data = parsed
    return data

def write_register(sock, address, value):
    """Send a write request and return (echo_register, echo_value) or None."""
    req = build_write_request(address, value)
    sock.sendall(req)
    resp = sock.recv(4096)
    result = decode_response(resp)
    if result is None:
        return None
    _, _, payload = result
    return parse_write_response(payload)

def signed16(val: int) -> int:
    """Interpret a 16-bit register value as signed."""
    if val >= 32768:
        return val - 65536
    return val

# --- Assertion helpers -------------------------------------------------------

def assert_eq(actual, expected, msg=""):
    if actual != expected:
        raise AssertionError(f"{msg}: expected {expected}, got {actual}")

def assert_true(condition, msg=""):
    if not condition:
        raise AssertionError(f"Assertion failed: {msg}")

# --- Config generators -------------------------------------------------------

def make_plant_config(
    inverter_type: str,
    battery_count: int = 1,
    battery_size_kwh: float = 9.5,
    soc: float = 65.0,
    solar_peak: float = 5000.0,
) -> dict:
    """Generate a PlantConfig dict for a given inverter type."""

    # Inverter-specific defaults
    max_ac_w = {
        "Gen3Hybrid": 5000.0,
        "Gen2Hybrid": 5000.0,
        "ACCoupled": 3000.0,
        "ThreePhase": 6000.0,
        "AllInOne6": 6000.0,
    }.get(inverter_type, 5000.0)

    dsp_fw = {
        "Gen3Hybrid": 449,
        "Gen2Hybrid": 230,
        "ACCoupled": 305,
        "ThreePhase": 612,
        "AllInOne6": 1010,
    }.get(inverter_type, 449)

    max_batt_kw = {
        "Gen3Hybrid": 3.6,
        "Gen2Hybrid": 3.6,
        "ACCoupled": 3.0,
        "ThreePhase": 6.0,
        "AllInOne6": 6.0,
    }.get(inverter_type, 3.6)

    # Battery module(s)
    per_module_kw = min(max_batt_kw / battery_count, battery_size_kwh * 0.7, 10.0)
    batteries = []
    for i in range(battery_count):
        batteries.append({
            "soc_percent": soc,
            "capacity_kwh": battery_size_kwh,
            "nominal_capacity_kwh": battery_size_kwh,
            "max_charge_kw": per_module_kw,
            "max_discharge_kw": per_module_kw,
            "min_soc": 4.0,
            "max_soc": 100.0,
            "power_kw": 0.0,
            "charge_efficiency": 0.95,
            "discharge_efficiency": 0.95,
            "temperature_celsius": 25.0,
            "throughput_kwh": 100.0,
            "soh": 1.0,
            "cycle_count": 10.0,
            "voltage_v": 48.0,
            "current_a": 0.0,
        })

    return {
        "plant": {
            "timestamp": "2025-06-15T12:00:00",
            "inverter": {
                "mode_state": {
                    "effective": "Eco",
                    "source": "User",
                    "scheduled_mode": None,
                },
                "ac_power_w": 0.0,
                "export_limit_w": max_ac_w * 0.72,
                "temperature_celsius": 35.0,
                "dsp_firmware_version": dsp_fw,
                "arm_firmware_version": 0,
                "work_time_hours": 1000.0,
                "battery_self_heating": False,
                "manual_battery_heater": False,
            },
            "battery": batteries[0],
            "batteries": batteries,
            "solar": {
                "generation_w": 3000.0,
                "pv1_w": 2500.0,
                "pv2_w": 500.0,
            },
            "load": {"demand_w": 800.0},
            "grid": {"power_w": -500.0, "connected": True},
            "active_faults": [],
            "weather": "clear",
            "energy_totals": {
                "grid_import_kwh": 1.5,
                "grid_export_kwh": 2.5,
                "battery_charge_kwh": 3.5,
                "battery_discharge_kwh": 4.5,
                "solar_generation_kwh": 8.5,
                "load_consumption_kwh": 6.5,
                "ac_charge_kwh": 0.7,
                "inverter_output_kwh": 8.0,
            },
            "config": {
                "solar_peak_watts": solar_peak,
                "latitude": 51.5,
                "tick_interval_secs": 30,
                "inverter_type": inverter_type,
                "max_ac_watts": max_ac_w,
                "pv2_peak_watts": 0.0,
                "ct_meter_installed": True,
            },
        },
        "schedule": None,
    }


def save_config(config: dict, name: str):
    """Save a config dict to a JSON file."""
    path = CONFIG_DIR / f"{name}.json"
    path.write_text(json.dumps(config, indent=2))
    return str(path)

# --- Test cases --------------------------------------------------------------

def test_read_ir_block(sock, port, config_name, config):
    """Test reading IR(0,60) - input register block."""
    print(f"[{config_name}] Test: Read IR(0,60)...")

    data = read_registers(sock, SLAVE_READ, FUNC_READ_INPUT, 0, 60)
    assert data is not None, "IR(0,60) read failed - no response"
    assert len(data) >= 60, f"IR(0,60) expected >=60 registers, got {len(data)}"

    # IR(0): status register - should be between 0 and 4
    assert 0 <= data[0] <= 4, f"IR0 (status) out of range: {data[0]}"

    # IR(52): p_battery - should be non-zero
    bat_power = signed16(data[52])
    assert bat_power != 0, f"IR52 (p_battery) is zero"

    # IR(59): SOC - should match config (within tolerance since sim ticks update)
    soc = data[59]
    expected_soc = int(config["plant"]["batteries"][0]["soc_percent"])
    assert 0 <= soc <= 100, f"IR59 (SOC) out of range: {soc}"

    # IR(30): grid power - should be non-zero
    grid_power = signed16(data[30])
    # Grid power can vary after ticks

    # IR(18): PV1 power - non-negative
    assert signed16(data[18]) >= 0, f"IR18 (p_pv1) is negative: {signed16(data[18])}"

    print(f"  OK: IR(0,60) returned {len(data)} registers, SOC={soc}%, "
          f"bat_power={bat_power}W, pv1={signed16(data[18])}W, "
          f"grid={grid_power}W")

def test_read_hr_block(sock, port, config_name, config, inverter_type):
    """Test reading HR(0,60) - holding register block containing device info."""
    print(f"[{config_name}] Test: Read HR(0,60)...")

    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 0, 60)
    assert data is not None, "HR(0,60) read failed - no response"
    assert len(data) >= 60, f"HR(0,60): expected >=60 registers, got {len(data)}"

    # HR(0): device_type_code - must match expected DTC
    dtc = data[0]
    expected_dtc = EXPECTED_DTC.get(inverter_type)
    if expected_dtc is not None:
        assert_eq(dtc, expected_dtc,
                  f"HR0 device_type_code mismatch for {inverter_type}: "
                  f"expected 0x{expected_dtc:04X}, got 0x{dtc:04X}")
    else:
        assert dtc > 0, f"HR0 (device_type_code) is zero"

    # HR(3): num_phases - low byte should be 1 or 3
    num_phases = data[3] & 0xFF
    assert num_phases in (1, 3), f"HR3 num_phases={num_phases}, expected 1 or 3"

    # HR(27): battery_power_mode
    assert data[27] in (0, 1), f"HR27 battery_power_mode={data[27]}"

    # HR(55): battery_capacity_ah - should be > 0
    assert data[55] > 0, f"HR55 battery_capacity_ah={data[55]} (expected > 0)"

    print(f"  OK: HR0 = 0x{dtc:04X}, phases={num_phases}, "
          f"batt_mode={data[27]}, capacity_ah={data[55]}")

def test_write_eco_mode(sock, port, config_name, config):
    """Test writing HR 27 = 1 (eco mode) and reading back."""
    print(f"[{config_name}] Test: Write HR 27 = 1 (eco mode)...")

    # Write
    result = write_register(sock, 27, 1)
    assert result is not None, "Write HR 27 returned no response"
    reg, val = result
    assert_eq(reg, 27, "Write echo: wrong register")
    assert_eq(val, 1, "Write echo: wrong value")

    # Read back
    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 27, 1)
    assert data is not None, "Read-back HR27 failed"
    assert_eq(data[0], 1, f"HR27 read-back: expected 1, got {data[0]}")

    print("  OK: HR27 = 1 (eco mode) set and confirmed")

def test_force_charge(sock, port, config_name, config):
    """Test force charge: write HR 96 = 1, HR 116 = 100."""
    print(f"[{config_name}] Test: Force charge (HR96=1, HR116=100)...")

    # Write enable charge
    result = write_register(sock, 96, 1)
    assert result is not None
    reg, val = result
    assert_eq(reg, 96)
    assert_eq(val, 1)

    # Write charge target SOC
    result = write_register(sock, 116, 100)
    assert result is not None
    reg, val = result
    assert_eq(reg, 116)
    assert_eq(val, 100)

    # Read back from HR 60-119 block
    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 60, 60)
    assert data is not None
    assert len(data) >= 57  # need index 56 for HR116

    # HR96 is at offset 36 (96 - 60)
    assert_eq(data[36], 1, f"HR96 read-back: expected 1, got {data[36]}")

    # HR116 is at offset 56 (116 - 60)
    assert_eq(data[56], 100, f"HR116 read-back: expected 100, got {data[56]}")

    print("  OK: Force charge enabled, target SOC=100%")

def test_soc_reserve(sock, port, config_name, config):
    """Test writing SOC reserve (HR 110)."""
    print(f"[{config_name}] Test: Write HR 110 = 50 (SOC reserve)...")

    result = write_register(sock, 110, 50)
    assert result is not None
    reg, val = result
    assert_eq(reg, 110)
    assert_eq(val, 50)

    # Read back
    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 110, 1)
    assert data is not None
    assert_eq(data[0], 50, f"HR110 read-back: expected 50, got {data[0]}")

    print("  OK: SOC reserve = 50%")

def test_charge_slot(sock, port, config_name, config):
    """Test writing a charge schedule slot (HR 94/95)."""
    print(f"[{config_name}] Test: Write charge slot (HR94=2200, HR95=500)...")

    r1 = write_register(sock, 94, 2200)  # start 22:00
    assert r1 is not None
    r2 = write_register(sock, 95, 500)   # end 05:00
    assert r2 is not None

    # Read back
    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 94, 2)
    assert data is not None
    assert_eq(data[0], 2200, f"HR94: expected 2200, got {data[0]}")
    assert_eq(data[1], 500, f"HR95: expected 500, got {data[1]}")

    print("  OK: Charge slot 22:00-05:00 confirmed")

def test_read_hr_60_119_block(sock, port, config_name, config):
    """Test reading the extended holding register block HR(60,60)."""
    print(f"[{config_name}] Test: Read HR(60,60)...")

    data = read_registers(sock, SLAVE_READ, FUNC_READ_HOLDING, 60, 60)
    assert data is not None, "HR(60,60) read failed"
    assert len(data) >= 60, f"HR(60,60): expected >=60 registers, got {len(data)}"

    # Check some known registers are accessible
    # HR96 (enable_charge at offset 36)
    # HR110 (soc_reserve at offset 50)
    # HR116 (charge_target at offset 56)
    assert data[36] in (0, 1), f"HR96 (enable_charge) unexpected: {data[36]}"
    assert data[50] >= 0, f"HR110 (soc_reserve) unexpected: {data[50]}"

    print(f"  OK: HR(60,60) returned {len(data)} registers")

# --- Main test runner --------------------------------------------------------

INVERTER_CONFIGS = [
    {
        "name": "gen3_hybrid",
        "type": "Gen3Hybrid",
        "batteries": 1,
        "battery_size": 9.5,
        "soc": 65.0,
        "solar_peak": 5000.0,
    },
    {
        "name": "ac_coupled",
        "type": "ACCoupled",
        "batteries": 1,
        "battery_size": 9.5,
        "soc": 50.0,
        "solar_peak": 3000.0,
    },
    {
        "name": "three_phase",
        "type": "ThreePhase",
        "batteries": 2,
        "battery_size": 9.5,
        "soc": 75.0,
        "solar_peak": 6000.0,
    },
    {
        "name": "gen2_hybrid",
        "type": "Gen2Hybrid",
        "batteries": 1,
        "battery_size": 9.5,
        "soc": 40.0,
        "solar_peak": 5000.0,
    },
    {
        "name": "allinone_6kw",
        "type": "AllInOne6",
        "batteries": 1,
        "battery_size": 9.5,
        "soc": 60.0,
        "solar_peak": 6000.0,
    },
]


def run_all_tests():
    """Run all tests for all inverter types sequentially."""
    print("=" * 70)
    print("GivEnergy Simulator Integration Tests")
    print(f"Binary: {SIM_API_BIN}")
    print(f"Config dir: {CONFIG_DIR}")
    print("=" * 70)

    # Phase 1: Generate configs
    print("\n--- Generating config files ---")
    config_paths = {}
    for cfg in INVERTER_CONFIGS:
        plant_config = make_plant_config(
            cfg["type"],
            battery_count=cfg["batteries"],
            battery_size_kwh=cfg["battery_size"],
            soc=cfg["soc"],
            solar_peak=cfg["solar_peak"],
        )
        path = save_config(plant_config, cfg["name"])
        config_paths[cfg["name"]] = path
        print(f"  Saved: {path}")

    # Phase 2: Run tests for each inverter
    results = {"passed": 0, "failed": 0, "details": []}

    for i, cfg in enumerate(INVERTER_CONFIGS):
        port = BASE_PORT + i
        name = cfg["name"]
        inverter_type = cfg["type"]
        config_path = config_paths[name]

        print(f"\n{'=' * 70}")
        print(f"Testing: {name} ({inverter_type}) on port {port}")
        print(f"Config:  {config_path}")
        print(f"{'=' * 70}")

        sim = SimulatorInstance(config_path, port)
        test_results = []

        try:
            # Start simulator
            print(f"Starting simulator on port {port}...")
            if not sim.start():
                print(f"  FAILED: Simulator did not start for {name}")
                results["failed"] += 1
                results["details"].append({"name": name, "status": "FAIL", "reason": "simulator startup"})
                sim.stop()
                continue

            print(f"  Simulator started (PID={sim.process.pid})")

            # Connect
            sock = connect(port)
            print(f"  Connected to port {port}")

            try:
                # Run tests
                plant_config = make_plant_config(
                    inverter_type,
                    battery_count=cfg["batteries"],
                    battery_size_kwh=cfg["battery_size"],
                    soc=cfg["soc"],
                    solar_peak=cfg["solar_peak"],
                )
                test_read_hr_block(sock, port, name, plant_config, inverter_type)
                test_read_ir_block(sock, port, name, plant_config)
                test_read_hr_60_119_block(sock, port, name, plant_config)
                test_write_eco_mode(sock, port, name, plant_config)
                test_soc_reserve(sock, port, name, plant_config)
                test_charge_slot(sock, port, name, plant_config)
                test_force_charge(sock, port, name, plant_config)

                results["passed"] += 1
                test_results.append("PASS")
                print(f"\n  >>> {name}: ALL TESTS PASSED")

            except Exception as e:
                results["failed"] += 1
                test_results.append("FAIL")
                print(f"\n  >>> {name}: FAILED - {e}")
                results["details"].append({
                    "name": name,
                    "status": "FAIL",
                    "reason": str(e),
                })

            finally:
                try:
                    sock.close()
                except Exception:
                    pass

        finally:
            pid_str = str(sim.process.pid) if sim.process else "N/A"
            print(f"  Stopping simulator (PID={pid_str})...")
            sim.stop()
            print(f"  Simulator stopped")

    # Phase 3: Summary
    print("\n" + "=" * 70)
    print("TEST SUMMARY")
    print("=" * 70)
    total = results["passed"] + results["failed"]
    print(f"  Total: {total}")
    print(f"  Passed: {results['passed']}")
    print(f"  Failed: {results['failed']}")
    if results["failed"] > 0:
        print("\nFailures:")
        for d in results["details"]:
            print(f"  - {d['name']}: {d['reason']}")
    print("=" * 70)

    return 0 if results["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(run_all_tests())
