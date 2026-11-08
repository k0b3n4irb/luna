#!/usr/bin/env python3
"""Unit tests of tools/oracle-json.py: sample logs in, JSON out.

    python3 tools/test_oracle_json.py
"""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("oracle_json", HERE / "oracle-json.py")
oj = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oj)

CONTESTED, _ = oj.contested_states()
FULL_816 = 512 * 10_000
RUN_816 = FULL_816 - 40_000  # the four MVN/MVP files are not run


def log_816(fail=CONTESTED, panicked=0, cyc_bad=0, result="ok"):
    passed = RUN_816 - fail - panicked
    tail = "test result: ok. 1 passed; 0 failed; 1 ignored" if result == "ok" else "test result: FAILED. 0 passed; 1 failed; 1 ignored"
    return f"""Reading Tom Harte tests from tests/tom-harte/v1
Found 508 JSON files

============================================
  Tom Harte 65C816 — results
============================================
  Files processed: 508
  Pass:    {passed}
  Fail:    {fail}
  Panicked: {panicked}

  Cycle-count backstop (io_cycle invocations == bus-cycle trace length):
    Match:    {RUN_816 - cyc_bad}
    Mismatch: {cyc_bad}

  Per-cycle bus-trace oracle (entry-for-entry kind+addr+value):
    Match:    4000000
    Diverge:  264   (expected: the TRACE_RESIDUAL files, every case)

{tail}; 0 measured; 0 filtered out
"""


def block_spc(name, fail=0):
    return f"""
============================================
  Tom Harte SPC700 — {name} core
============================================
  Files processed: 256
  Pass:    {256000 - fail}
  Fail:    {fail}
  Panicked: 0

  Cycle-trace check (entry-for-entry: read/write/idle, addr+value):
    Match:    256000
    Mismatch: 0
"""


def log_spc(fail=0, result="ok"):
    tail = "test result: ok. 1 passed; 0 failed; 1 ignored" if result == "ok" else "test result: FAILED. 0 passed; 1 failed; 1 ignored"
    return block_spc("production (step_cycle)", fail) + block_spc("atomic (Spc700::step)") + tail + "\n"


class Oracle(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def write(self, name, text):
        p = Path(self.tmp.name) / name
        p.write_text(text)
        return str(p)

    def gate(self, name, text, outcome=None):
        return oj.build_gate(name, self.write(name + ".log", text) if text is not None else None, outcome, "local")

    def test_816_green_baseline(self):
        g = self.gate("tom-harte-65816", log_816())
        self.assertEqual(g["result"], "pass", g)
        c = g["counts"]
        self.assertEqual(c["contested"], 246)
        self.assertEqual(c["states_total"], RUN_816)
        self.assertEqual(c["states_passed"], RUN_816 - 246)
        self.assertEqual(c["not_run"], 40_000)

    def test_816_faulted_state_is_fail(self):
        # one state failure beyond the contested baseline
        self.assertEqual(self.gate("tom-harte-65816", log_816(fail=CONTESTED + 1))["result"], "fail")

    def test_816_faulted_cycle_count_is_fail(self):
        self.assertEqual(self.gate("tom-harte-65816", log_816(cyc_bad=3))["result"], "fail")

    def test_816_panic_is_fail(self):
        self.assertEqual(self.gate("tom-harte-65816", log_816(panicked=1))["result"], "fail")

    def test_816_cargo_failed_is_fail(self):
        self.assertEqual(self.gate("tom-harte-65816", log_816(result="FAILED"))["result"], "fail")

    def test_816_truncated_log_is_fail(self):
        self.assertEqual(self.gate("tom-harte-65816", "Reading Tom Harte tests\n")["result"], "fail")

    def test_ci_failure_outcome_overrides_green_log(self):
        self.assertEqual(self.gate("tom-harte-65816", log_816(), "failure")["result"], "fail")
        self.assertEqual(self.gate("tom-harte-65816", log_816(), "cancelled")["result"], "fail")

    def test_ci_success_without_log_is_not_a_pass(self):
        self.assertEqual(self.gate("tom-harte-65816", None, "success")["result"], "fail")

    def test_no_log_no_outcome_is_skipped(self):
        self.assertEqual(self.gate("tom-harte-spc700", None)["result"], "skipped")
        self.assertEqual(self.gate("tom-harte-spc700", None, "skipped")["result"], "skipped")

    def test_sampled_run_is_not_a_gate(self):
        g = self.gate("tom-harte-65816", "Sampling first 10 case(s) per opcode file\n" + log_816())
        self.assertEqual(g["result"], "skipped")

    def test_spc700_green_and_faulted(self):
        g = self.gate("tom-harte-spc700", log_spc())
        self.assertEqual(g["result"], "pass", g)
        self.assertEqual(set(g["counts"]), {"production", "atomic"})
        self.assertEqual(g["counts"]["production"]["states_total"], 256000)
        self.assertEqual(self.gate("tom-harte-spc700", log_spc(fail=1))["result"], "fail")
        self.assertEqual(self.gate("tom-harte-spc700", log_spc(result="FAILED"))["result"], "fail")

    def test_rom_suite(self):
        ok = "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured\n"
        bad = "test result: FAILED. 11 passed; 1 failed; 0 ignored; 0 measured\n"
        g = self.gate("snes-test-roms", ok)
        self.assertEqual((g["result"], g["counts"]["passed"]), ("pass", 12))
        self.assertEqual(self.gate("snes-test-roms", bad)["result"], "fail")
        self.assertEqual(self.gate("snes-test-roms", "compiling\n")["result"], "fail")

    def test_doc_drift(self):
        self.assertEqual(self.gate("doc-drift", "[links] ok\ncheck-doc-drift: OK\n")["result"], "pass")
        g = self.gate("doc-drift", "[links] FAIL\ncheck-doc-drift: 3 finding(s)\n")
        self.assertEqual((g["result"], g["counts"]["findings"]), ("fail", 3))

    def test_document_shape(self):
        import argparse
        a = argparse.Namespace(
            tom_harte_65816=self.write("a.log", log_816()), tom_harte_spc700=self.write("b.log", log_spc()),
            snes_test_roms=None, doc_drift=None, outcome=["tom-harte-65816=success"],
            commit="0123456789abcdef0123456789abcdef01234567", now="2026-11-08T12:00:00Z", out="-",
        )
        doc = json.loads(json.dumps(oj.build(a)))
        self.assertEqual(doc["schema"], 1)
        self.assertEqual(doc["generated_at"], "2026-11-08T12:00:00Z")
        self.assertEqual(doc["commit"], a.commit)
        self.assertEqual([g["name"] for g in doc["gates"]], oj.GATE_NAMES)
        self.assertEqual({g["name"]: g["result"] for g in doc["gates"]},
                         {"tom-harte-65816": "pass", "tom-harte-spc700": "pass", "snes-test-roms": "skipped", "doc-drift": "skipped"})
        self.assertEqual(len(doc["not_proven"]), 4)
        self.assertRegex(doc["luna_version"], r"^\d+\.\d+\.\d+")
        for g in doc["gates"]:
            self.assertEqual(g["run_url"], "local")
            self.assertIn(g["result"], ("pass", "fail", "skipped"))

    def test_bad_outcome_name_is_refused(self):
        import argparse
        a = argparse.Namespace(tom_harte_65816=None, tom_harte_spc700=None, snes_test_roms=None, doc_drift=None,
                               outcome=["nope=success"], commit="x", now="t", out="-")
        with self.assertRaises(SystemExit):
            oj.build(a)


if __name__ == "__main__":
    unittest.main()
