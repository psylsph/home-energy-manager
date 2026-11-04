#!/usr/bin/env python3
"""Tests for scripts/check-givtcp-contract.py, offline.

GivTCP's files are stood in for by small sources with the same shape, so
these never touch the network. Also checks that the simulator's copy of the
contract is HEM's, when the simulator is checked out next to HEM.
"""

import copy
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-givtcp-contract.py"
CONTRACT = ROOT / "tests" / "fixtures" / "givtcp-model-contract.json"
SIMULATOR_COPY = (
    Path(os.environ.get("GIVENERGY_SIMULATOR_DIR", Path.home() / "repos" / "givenergy-simulator"))
    / "crates" / "sim-registers" / "tests" / "fixtures" / "givtcp-model-contract.json"
)

spec = importlib.util.spec_from_file_location("check_givtcp_contract", SCRIPT)
assert spec and spec.loader
checker = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = checker
spec.loader.exec_module(checker)

PAUSE_REGISTERS = '''
def pause_registers(model, arm_fw):
    """The pause registers this model can write"""
    if model == Model.AC:
        if arm_fw is not None and int(arm_fw) >= AC_PAUSE_MIN_ARM_FW:
            return frozenset({PAUSE_MODE_REGISTER})
        return frozenset()
    if model in PAUSE_MODELS:
        return PAUSE_REGISTERS
    if model == Model.HYBRID_GEN1 and arm_fw is not None and int(arm_fw) >= GEN1_PAUSE_MIN_ARM_FW:
        return frozenset({PAUSE_MODE_REGISTER})
    return frozenset()
'''

PATCHES = '''
GEN1_PAUSE_MIN_ARM_FW = 187
AC_PAUSE_MIN_ARM_FW = 200
PAUSE_MODELS = frozenset({Model.ALL_IN_ONE, Model.GATEWAY, Model.HYBRID_GEN2, Model.HYBRID_GEN3,
                          Model.HYBRID_HV_GEN3, Model.ALL_IN_ONE_HYBRID})
''' + PAUSE_REGISTERS + '''
manifest.CAPABILITIES["is_three_phase"] = manifest.CAPABILITIES["is_three_phase"] - {Model.ALL_IN_ONE_HYBRID}
'''

MANIFEST = '''
CAPABILITIES = {
    "is_three_phase": frozenset(
        {
            Model.HYBRID_3PH,
            Model.AC_3PH,
            Model.AIO_COMMERCIAL,
            Model.ALL_IN_ONE_HYBRID,
        }
    ),
    "is_ac_coupled": frozenset({Model.AC, Model.AC_3PH}),
}
'''

FORK = "a556333bb7ac1589d7c266ed9cdf5cdfafba44e8"
REQUIREMENTS = (
    "givenergy-modbus @ https://github.com/britkat1980/givenergy-modbus/archive/"
    f"{FORK}.zip \\\n    --hash=sha256:abc\n"
)


def contract_for(patches_text):
    """The real contract, with the fingerprint of the stand-in source."""
    contract = json.loads(CONTRACT.read_text())
    contract["givtcp"]["pause_registers_fingerprint"] = checker.parse_patches(patches_text)[
        "pause_registers_fingerprint"
    ]
    return contract


def run(contract, patches=PATCHES, manifest=MANIFEST, requirements=REQUIREMENTS):
    return checker.check(
        contract,
        checker.parse_patches(patches),
        checker.parse_manifest(manifest),
        checker.pinned_fork(requirements),
    )


class ContractCheck(unittest.TestCase):
    def test_the_contract_matches_these_givtcp_rules(self):
        self.assertEqual(run(contract_for(PATCHES)), [])

    def test_a_new_ac_firmware_floor_is_caught(self):
        patches = PATCHES.replace("AC_PAUSE_MIN_ARM_FW = 200", "AC_PAUSE_MIN_ARM_FW = 214")
        problems = run(contract_for(patches), patches=patches)
        self.assertTrue(any("AC_PAUSE_MIN_ARM_FW is 214" in p for p in problems), problems)
        self.assertTrue(any("0x3001 ARM 200" in p for p in problems), problems)

    def test_a_model_joining_the_pause_list_is_caught(self):
        patches = PATCHES.replace("Model.HYBRID_GEN2,", "Model.HYBRID_GEN2, Model.HYBRID_GEN4,")
        problems = run(contract_for(patches), patches=patches)
        self.assertTrue(any("PAUSE_MODELS" in p for p in problems), problems)
        self.assertTrue(any("0x8304" in p and "mode_and_window" in p for p in problems), problems)

    def test_a_change_to_pause_registers_logic_is_caught(self):
        changed = PATCHES.replace("int(arm_fw) >= AC_PAUSE_MIN_ARM_FW", "int(arm_fw) > AC_PAUSE_MIN_ARM_FW")
        problems = run(contract_for(PATCHES), patches=changed)
        self.assertTrue(any("pause_registers() has changed" in p for p in problems), problems)

    def test_comments_and_layout_in_pause_registers_dont_count(self):
        reworded = PATCHES.replace(
            '"""The pause registers this model can write"""', '"""Which pause registers."""\n    # a comment'
        )
        self.assertEqual(run(contract_for(PATCHES), patches=reworded), [])

    def test_dropping_the_aio_hybrid_patch_is_caught(self):
        patches = "\n".join(l for l in PATCHES.splitlines() if "is_three_phase" not in l)
        problems = run(contract_for(patches), patches=patches)
        self.assertTrue(any("0x8201" in p and "three-phase is true" in p for p in problems), problems)

    def test_a_new_fork_pin_is_caught(self):
        requirements = REQUIREMENTS.replace(FORK, "b" * 40)
        problems = run(contract_for(PATCHES), requirements=requirements)
        self.assertTrue(any("now pins" in p for p in problems), problems)

    def test_a_contract_row_that_disagrees_is_caught(self):
        contract = contract_for(PATCHES)
        row = next(r for r in contract["rows"] if r["code"] == "8001")
        row["givtcp"]["pause"] = "mode"
        problems = run(contract)
        self.assertTrue(any("0x8001" in p for p in problems), problems)

    def test_a_reshaped_source_is_a_read_error_not_a_pass(self):
        with self.assertRaises(checker.SourceError):
            checker.parse_patches("AC_PAUSE_MIN_ARM_FW = 200\n")
        with self.assertRaises(checker.SourceError):
            checker.parse_manifest("CAPABILITIES = {}\n")
        with self.assertRaises(checker.SourceError):
            checker.pinned_fork("givenergy-modbus==2.13.0\n")

    def test_main_reads_a_directory_and_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            (base / "modbus_patches.py").write_text(PATCHES)
            (base / "requirements.txt").write_text(REQUIREMENTS)
            (base / "manifest.py").write_text(MANIFEST)
            contract = base / "contract.json"
            contract.write_text(json.dumps(contract_for(PATCHES)))
            self.assertEqual(checker.main(["--dir", tmp, "--contract", str(contract)]), 0)
            broken = contract_for(PATCHES)
            broken["constants"]["GEN1_PAUSE_MIN_ARM_FW"] = 186
            contract.write_text(json.dumps(broken))
            self.assertEqual(checker.main(["--dir", tmp, "--contract", str(contract)]), 1)
            (base / "manifest.py").unlink()
            self.assertEqual(checker.main(["--dir", tmp, "--contract", str(contract)]), 2)


class SimulatorCopy(unittest.TestCase):
    def test_the_simulator_holds_the_same_contract(self):
        if not SIMULATOR_COPY.exists():
            self.skipTest(f"no simulator checkout at {SIMULATOR_COPY.parents[4]}")
        self.assertEqual(
            json.loads(SIMULATOR_COPY.read_text()),
            json.loads(CONTRACT.read_text()),
            f"copy tests/fixtures/givtcp-model-contract.json to {SIMULATOR_COPY}",
        )


class ContractShape(unittest.TestCase):
    def test_every_deviation_names_an_issue_and_a_reason(self):
        contract = json.loads(CONTRACT.read_text())
        for row in contract["rows"]:
            for side in ("hem_deviations", "sim_deviations"):
                for field, deviation in row.get(side, {}).items():
                    with self.subTest(code=row["code"], arm_fw=row["arm_fw"], side=side, field=field):
                        self.assertGreater(deviation["issue"], 0)
                        self.assertTrue(deviation["why"].strip())
                        self.assertNotEqual(deviation["value"], copy.deepcopy(row["givtcp"]).get(field, object()))


if __name__ == "__main__":
    unittest.main()
