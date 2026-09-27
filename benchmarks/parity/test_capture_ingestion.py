import json
import unittest
import xml.etree.ElementTree as ET

from capture_ingestion import cycl_fixture, owl_fixture, validate_stdout


class IngestionCaptureTests(unittest.TestCase):
    def test_fixture_has_distinct_subjects_and_expected_fact_counts(self):
        document = ET.fromstring(owl_fixture(7))
        self.assertEqual(len(document), 8)
        self.assertEqual(sum(len(subject) for subject in document), 15)
        self.assertEqual(len({tuple(subject.attrib.values()) for subject in document}), 8)
        self.assertIn('é🦀', document[0][0].text)
        lines = cycl_fixture(9).splitlines()
        self.assertEqual(len(lines), 9)
        self.assertEqual(sum('(#$isa ' in line for line in lines), 5)
        self.assertEqual(sum('(#$comment ' in line for line in lines), 4)

    def test_rejects_partial_or_changed_results(self):
        good = {'subjects': 8, 'total_subjects': 8, 'asserted': 15, 'complete': True,
                'dropped_resources': 0, 'dropped_repeats': 0, 'durability': 'strict'}
        validate_stdout('owl', 'rust', json.dumps(good), 7, 9)
        for key, value in [('subjects', 7), ('complete', False), ('asserted', 14),
                           ('dropped_resources', 1), ('durability', 'relaxed')]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_stdout('owl', 'rust', json.dumps({**good, key: value}), 7, 9)
        validate_stdout('owl', 'odin', 'done: 8 subjects, 15 triples, 20 internal', 7, 9)
        with self.assertRaises(ValueError):
            validate_stdout('owl', 'odin', 'done: 7 subjects, 15 triples, 20 internal', 7, 9)
        census = '9 assertions parsed, 0 lines skipped; 2 predicates:\n4\t#$comment\n5\t#$isa\n'
        validate_stdout('cycl', 'odin', census, 7, 9)
        with self.assertRaises(ValueError):
            validate_stdout('cycl', 'odin', census.replace('5\t', '4\t'), 7, 9)
