"""Run with the optional pinned environment; never imported by default CI."""

import asyncio
import json
import os
from pathlib import Path
import tempfile
import unittest

from run import EXPECTED, run_case, validate_result


class EvidenceTests(unittest.TestCase):
    def test_exit_zero_alone_is_not_evidence(self):
        self.assertFalse(validate_result(0, b"", EXPECTED["1.6"], "1.6"))
        self.assertFalse(validate_result(0, b'{"event":"run_failed"}',
                                         EXPECTED["1.6"], "1.6"))

    def test_missing_duplicate_or_rejected_exchange_cannot_pass(self):
        output = b'{"event":"run_passed"}'
        for protocol, expected in EXPECTED.items():
            self.assertTrue(validate_result(0, output, expected, protocol))
            self.assertFalse(validate_result(3, output, expected, protocol))
            self.assertFalse(validate_result(0, output, expected[:-1], protocol))
            self.assertFalse(validate_result(0, output, expected + expected[:1], protocol))
            rejected = [action.replace(":Accepted", ":Rejected") for action in expected]
            self.assertFalse(validate_result(0, output, rejected, protocol))

    def test_real_binary_and_external_stack(self):
        binary = Path(os.environ["UOB_SIM"]).resolve(strict=True)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            for protocol in EXPECTED:
                result = asyncio.run(run_case(binary, protocol, False, output))
                self.assertEqual(result["status"], "passed", result)
                rejected = asyncio.run(run_case(binary, protocol, True, output))
                self.assertEqual(rejected["status"], "failed", rejected)
                self.assertEqual(rejected["simulator_exit_code"], 3)
                self.assertEqual(rejected["failure_code"], "unexpected_protocol_response")
                self.assertEqual(rejected["observed"], ["BootNotification"])
                events = [json.loads(line) for line in
                          (output / f"{protocol}-mismatch.jsonl").read_text().splitlines()]
                self.assertEqual(events[-1]["event"], "run_failed")


if __name__ == "__main__":
    unittest.main()
