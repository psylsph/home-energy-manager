#!/usr/bin/env python3
"""Check tests/fixtures/givtcp-model-contract.json against GivTCP itself.

The contract records what GivTCP does per inverter model (which pause
registers it writes, which models it treats as three-phase), and HEM's and
the simulator's tests hold them to it offline. This re-reads GivTCP, so a
change there shows up before a release rather than as a user's bug report:

    npm run check:givtcp                  # GivTCP's master branch, over the network
    npm run check:givtcp -- --ref <sha>   # a given GivTCP commit
    npm run check:givtcp -- --dir <path>  # files already on disk (see --help)

It reads GivTCP/modbus_patches.py and requirements.txt from GivTCP, and
givenergy_modbus/model/manifest.py from the givenergy-modbus fork commit
GivTCP pins. The files are parsed with `ast`, never imported or run.
GivTCP's pause_registers() is fingerprinted, so a change to its logic, not
just its constants, fails the check until the contract has been re-derived
(--fingerprint prints the new value).

Exit status: 0 when the contract matches, 1 when it doesn't, 2 when GivTCP
can't be read.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "tests" / "fixtures" / "givtcp-model-contract.json"
RAW = "https://raw.githubusercontent.com/{repo}/{ref}/{path}"
GIVTCP_REPO = "britkat1980/giv_tcp"


class SourceError(Exception):
    """GivTCP's files couldn't be read, or don't have the expected shape."""


# -- Parsing ------------------------------------------------------------------


def _model_names(node: ast.AST) -> set[str]:
    """`frozenset({Model.A, Model.B})` or `{Model.A}` -> {"A", "B"}."""
    if isinstance(node, ast.Call) and node.args:
        node = node.args[0]
    if not isinstance(node, ast.Set):
        raise SourceError(f"expected a set of models, got {ast.dump(node)[:80]}")
    names = set()
    for elt in node.elts:
        if not (isinstance(elt, ast.Attribute) and isinstance(elt.value, ast.Name) and elt.value.id == "Model"):
            raise SourceError(f"expected Model.<name>, got {ast.dump(elt)[:80]}")
        names.add(elt.attr)
    return names


def _top_level_assignments(tree: ast.Module) -> dict[str, ast.AST]:
    out = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            out[node.targets[0].id] = node.value
    return out


def fingerprint(func: ast.FunctionDef) -> str:
    """A hash of a function's logic, blind to comments, docstrings and layout."""
    body = list(func.body)
    if body and isinstance(body[0], ast.Expr) and isinstance(getattr(body[0], "value", None), ast.Constant):
        body = body[1:]
    parts = [ast.dump(func.args, include_attributes=False)]
    parts += [ast.dump(stmt, include_attributes=False) for stmt in body]
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()


def parse_patches(text: str) -> dict:
    """The pause rules and the three-phase patch from GivTCP/modbus_patches.py."""
    tree = ast.parse(text)
    assigns = _top_level_assignments(tree)
    try:
        ac_min = ast.literal_eval(assigns["AC_PAUSE_MIN_ARM_FW"])
        gen1_min = ast.literal_eval(assigns["GEN1_PAUSE_MIN_ARM_FW"])
        pause_models = _model_names(assigns["PAUSE_MODELS"])
    except KeyError as missing:
        raise SourceError(f"modbus_patches.py no longer defines {missing}") from None
    funcs = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "pause_registers"]
    if not funcs:
        raise SourceError("modbus_patches.py no longer defines pause_registers()")

    # manifest.CAPABILITIES["is_three_phase"] = manifest.CAPABILITIES["is_three_phase"] - {Model.X}
    removed: set[str] = set()
    for node in tree.body:
        if not (isinstance(node, ast.Assign) and len(node.targets) == 1):
            continue
        target = node.targets[0]
        if not (isinstance(target, ast.Subscript) and ast.unparse(target).endswith("CAPABILITIES['is_three_phase']")):
            continue
        value = node.value
        if not (isinstance(value, ast.BinOp) and isinstance(value.op, ast.Sub)):
            raise SourceError("modbus_patches.py changes is_three_phase in a way this check doesn't understand")
        removed |= _model_names(value.right)
    return {
        "AC_PAUSE_MIN_ARM_FW": ac_min,
        "GEN1_PAUSE_MIN_ARM_FW": gen1_min,
        "PAUSE_MODELS": pause_models,
        "three_phase_removed": removed,
        "pause_registers_fingerprint": fingerprint(funcs[0]),
    }


def parse_manifest(text: str) -> set[str]:
    """The fork's CAPABILITIES["is_three_phase"] models."""
    tree = ast.parse(text)
    for node in ast.walk(tree):
        if isinstance(node, ast.Dict):
            for key, value in zip(node.keys, node.values):
                if isinstance(key, ast.Constant) and key.value == "is_three_phase":
                    return _model_names(value)
    raise SourceError("manifest.py no longer has an is_three_phase capability")


def pinned_fork(requirements: str) -> tuple[str, str]:
    """The givenergy-modbus fork repo and commit GivTCP's requirements pin."""
    match = re.search(r"github\.com/([\w.-]+/givenergy-modbus)/archive/([0-9a-f]{40})\.zip", requirements)
    if not match:
        raise SourceError("requirements.txt no longer pins givenergy-modbus to a GitHub commit")
    return match.group(1), match.group(2)


# -- GivTCP's rules -------------------------------------------------------------


def givtcp_pause(model: str, arm_fw: int, rules: dict) -> str:
    """GivTCP's pause_registers() for one model, as the contract names it.

    Mirrors the function whose fingerprint is in the contract: if GivTCP
    changes it, the fingerprint check fails before this can mislead.
    """
    if model == "AC":
        return "mode" if arm_fw >= rules["AC_PAUSE_MIN_ARM_FW"] else "none"
    if model in rules["PAUSE_MODELS"]:
        return "mode_and_window"
    if model == "HYBRID_GEN1" and arm_fw >= rules["GEN1_PAUSE_MIN_ARM_FW"]:
        return "mode"
    return "none"


def check(contract: dict, patches: dict, fork_three_phase: set[str], fork: tuple[str, str]) -> list[str]:
    """Every way the contract disagrees with GivTCP, as readable lines."""
    problems = []
    three_phase = fork_three_phase - patches["three_phase_removed"]
    expected_fork = (contract["modbus_fork"]["repo"], contract["modbus_fork"]["commit"])
    if fork != expected_fork:
        problems.append(
            f"GivTCP now pins {fork[0]} at {fork[1]}, the contract {expected_fork[1]}: "
            "re-read its manifest.py, then update modbus_fork"
        )
    recorded = contract["givtcp"].get("pause_registers_fingerprint")
    if recorded != patches["pause_registers_fingerprint"]:
        problems.append(
            "GivTCP's pause_registers() has changed: re-derive the pause rules, update "
            f"givtcp_pause() here, and set pause_registers_fingerprint to {patches['pause_registers_fingerprint']}"
        )
    constants = contract["constants"]
    for name in ("AC_PAUSE_MIN_ARM_FW", "GEN1_PAUSE_MIN_ARM_FW"):
        if constants[name] != patches[name]:
            problems.append(f"{name} is {patches[name]} in GivTCP, {constants[name]} in the contract")
    if set(constants["PAUSE_MODELS"]) != patches["PAUSE_MODELS"]:
        problems.append(
            f"PAUSE_MODELS is {sorted(patches['PAUSE_MODELS'])} in GivTCP, {sorted(constants['PAUSE_MODELS'])} in the contract"
        )
    if set(constants["is_three_phase"]) != three_phase:
        problems.append(
            f"GivTCP treats {sorted(three_phase)} as three-phase, the contract {sorted(constants['is_three_phase'])}"
        )
    rules = patches
    for row in contract["rows"]:
        label = f"0x{row['code']} ARM {row['arm_fw']} ({row['givtcp_model']})"
        pause = givtcp_pause(row["givtcp_model"], row["arm_fw"], rules)
        if row["givtcp"]["pause"] != pause:
            problems.append(f"{label}: GivTCP's pause is {pause}, the contract says {row['givtcp']['pause']}")
        is_3ph = row["givtcp_model"] in three_phase
        if row["givtcp"]["three_phase"] != is_3ph:
            problems.append(
                f"{label}: GivTCP's three-phase is {str(is_3ph).lower()}, the contract says "
                f"{str(row['givtcp']['three_phase']).lower()}"
            )
    return problems


# -- Reading GivTCP -------------------------------------------------------------


def fetch(repo: str, ref: str, path: str) -> str:
    url = RAW.format(repo=repo, ref=ref, path=path)
    request = urllib.request.Request(url, headers={"User-Agent": "home-energy-manager-contract-check"})
    try:
        with urllib.request.urlopen(request, timeout=30) as resp:
            return resp.read().decode("utf-8")
    except OSError as err:
        raise SourceError(f"couldn't fetch {url}: {err}") from None


def read_sources(args: argparse.Namespace) -> tuple[str, str, str]:
    """modbus_patches.py, requirements.txt and the pinned fork's manifest.py."""
    if args.dir:
        base = Path(args.dir)
        try:
            return tuple((base / name).read_text() for name in ("modbus_patches.py", "requirements.txt", "manifest.py"))
        except OSError as err:
            raise SourceError(str(err)) from None
    patches = fetch(GIVTCP_REPO, args.ref, "GivTCP/modbus_patches.py")
    requirements = fetch(GIVTCP_REPO, args.ref, "requirements.txt")
    fork_repo, fork_commit = pinned_fork(requirements)
    manifest = fetch(fork_repo, fork_commit, "givenergy_modbus/model/manifest.py")
    return patches, requirements, manifest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ref", default="master", help="GivTCP branch, tag or commit (default: master)")
    parser.add_argument(
        "--dir",
        help="read modbus_patches.py, requirements.txt and manifest.py from this directory instead",
    )
    parser.add_argument("--contract", default=str(CONTRACT), help=argparse.SUPPRESS)
    parser.add_argument("--fingerprint", action="store_true", help="print pause_registers()'s fingerprint and stop")
    args = parser.parse_args(argv)

    try:
        patches_text, requirements, manifest_text = read_sources(args)
        patches = parse_patches(patches_text)
        if args.fingerprint:
            print(patches["pause_registers_fingerprint"])
            return 0
        problems = check(
            json.loads(Path(args.contract).read_text()),
            patches,
            parse_manifest(manifest_text),
            pinned_fork(requirements),
        )
    except SourceError as err:
        print(f"check-givtcp-contract: {err}", file=sys.stderr)
        return 2

    if problems:
        print("The GivTCP model contract is out of date:", file=sys.stderr)
        for line in problems:
            print(f"  - {line}", file=sys.stderr)
        print(
            "Update tests/fixtures/givtcp-model-contract.json (and the simulator's copy), then fix or record "
            "what HEM and the simulator do differently.",
            file=sys.stderr,
        )
        return 1
    print(f"The GivTCP model contract matches GivTCP ({args.dir or args.ref}).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
