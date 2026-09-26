import copy
import unittest

from capture import validate_report


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


if __name__ == "__main__":
    unittest.main()
