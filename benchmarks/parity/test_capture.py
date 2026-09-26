import copy
import unittest

from capture import fixture_protocol, validate_report


class CaptureContract(unittest.TestCase):
    def setUp(self):
        self.fixture = {"expected": "42"}
        self.protocol = {"workers": 1, "warmup": 2, "samples": 2, "iterations": 3}
        self.report = {
            "format": 1, "implementation": "rust", "expected": "42", "tier": "interpreter",
            "workers": 1, "relation_parallelism": 1, "accelerator": "disabled",
            "accelerator_placements": 0, "storage": "memory", "durability": "none", "authority": "root",
            "warmup_invocations": 2, "iterations_per_sample": 3, "timed_invocations": 6,
            "sample_elapsed_ns": [100, 200],
        }

    def test_rejects_incomparable_or_incomplete_measurements(self):
        for key, value in [("expected", "41"), ("workers", 8), ("tier", "native-enabled"),
                           ("timed_invocations", 5), ("iterations_per_sample", 4),
                           ("warmup_invocations", 0), ("durability", "strict"),
                           ("accelerator_placements", 1), ("sample_elapsed_ns", [100]),
                           ("sample_elapsed_ns", [100, 0]), ("sample_elapsed_ns", [100, True])]:
            report = copy.deepcopy(self.report)
            report[key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                validate_report(report, self.fixture, self.protocol, "rust", "interpreter")

    def test_accepts_complete_measurements(self):
        validate_report(self.report, self.fixture, self.protocol, "rust", "interpreter")

    def test_initial_derivation_cannot_become_a_warmed_scan(self):
        fixture = {**self.fixture, "invocation_mode": "single"}
        protocol = fixture_protocol(self.protocol, fixture)
        self.assertEqual(protocol, {"workers": 1, "warmup": 0, "samples": 1, "iterations": 1})
        self.assertEqual(self.protocol["iterations"], 3)
        with self.assertRaises(ValueError):
            validate_report(self.report, fixture, protocol, "rust", "interpreter")
        report = {**self.report, "warmup_invocations": 0, "iterations_per_sample": 1,
                  "timed_invocations": 1, "sample_elapsed_ns": [100]}
        validate_report(report, fixture, protocol, "rust", "interpreter")
        self.assertEqual(fixture_protocol(self.protocol, self.fixture), self.protocol)
        with self.assertRaises(ValueError):
            fixture_protocol(self.protocol, {"invocation_mode": "typo"})


if __name__ == "__main__":
    unittest.main()
