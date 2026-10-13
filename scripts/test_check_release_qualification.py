# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    'qualification', Path(__file__).with_name('check-release-qualification.py'))
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'report.txt').write_text('synthetic test evidence')
        self.commit = 'a' * 40
        self.ledger = qualification.template()
        self.ledger['candidate_commit'] = self.commit
        for gate in self.ledger['gates'].values():
            gate.update(status='passed', reviewer='test reviewer', evidence=[{
                'path': 'report.txt',
                'sha256': hashlib.sha256(b'synthetic test evidence').hexdigest(),
            }], started_at='2020-01-01T00:00:00Z', ended_at='2020-01-08T00:00:00Z')

    def check(self, phase='stable'):
        return qualification.check(self.ledger, self.root, phase, self.commit)

    def test_complete_record_is_accepted(self):
        self.assertEqual(self.check(), [])

    def test_unexecuted_template_blocks(self):
        self.ledger = qualification.template()
        self.assertGreaterEqual(len(self.check()), len(qualification.STABLE_GATES))

    def test_rc_does_not_require_post_publication_gates(self):
        self.ledger['gates']['tester-feedback']['status'] = 'untested'
        self.assertEqual(self.check('rc'), [])
        self.assertTrue(self.check())

    def test_short_soak_and_feedback_block(self):
        for key in ('workload-soak', 'tester-feedback'):
            self.ledger['gates'][key]['ended_at'] = '2020-01-01T01:00:00Z'
        self.assertEqual(len(self.check()), 2)

    def test_changed_missing_and_escaping_evidence_block(self):
        (self.root / 'report.txt').write_text('changed')
        self.assertTrue(self.check())
        (self.root / 'report.txt').unlink()
        self.assertTrue(self.check())
        self.ledger['gates']['workload-soak']['evidence'][0]['path'] = '../outside'
        self.assertTrue(any('inside the ledger' in error for error in self.check()))

    def test_stale_commit_and_unsigned_review_block(self):
        self.ledger['candidate_commit'] = 'b' * 40
        self.ledger['gates']['security-review']['reviewer'] = ''
        self.assertEqual(len(self.check()), 2)

    def test_malformed_records_block(self):
        self.ledger['gates']['security-review']['reviewer'] = 42
        self.ledger['gates']['workload-soak']['evidence'] = [None]
        self.ledger['gates']['tester-feedback']['ended_at'] = None
        self.assertEqual(len(self.check()), 3)

    def test_naive_future_and_reversed_windows_block(self):
        for end in ('2020-01-08T00:00:00', '2999-01-01T00:00:00Z', '2019-01-01T00:00:00Z'):
            self.ledger['gates']['workload-soak']['ended_at'] = end
            self.assertTrue(self.check())


if __name__ == '__main__':
    unittest.main()
