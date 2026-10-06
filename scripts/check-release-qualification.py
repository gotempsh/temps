#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Check a private release evidence ledger; never publish or promote a release."""

import argparse
import hashlib
import json
from datetime import datetime
from pathlib import Path

RC_GATES = (
    'integration-ci', 'fresh-install', 'upgrade-v0.0.8', 'rollback-v0.0.8',
    'workload-soak', 'security-review', 'external-penetration-test',
    'release-packaging', 'maturity-and-defaults',
)
STABLE_GATES = RC_GATES + (
    'tester-feedback', 'published-assets', 'stable-installer', 'release-notes',
    'support-window', 'promotion-approval',
)


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError('timestamp must be a string')
    parsed = datetime.fromisoformat(value.replace('Z', '+00:00'))
    if parsed.utcoffset() is None:
        raise ValueError('timestamp must include a timezone')
    return parsed


def template():
    return {
        'candidate_commit': '',
        'candidate_version': 'v0.1.0-rc.1',
        'gates': {key: {'status': 'untested', 'reviewer': '', 'evidence': []}
                  for key in STABLE_GATES},
    }


def check(ledger, root, phase, commit):
    """Validate recorded evidence integrity, not the truth of a human report."""
    problems = []
    if ledger.get('candidate_commit') != commit:
        problems.append('candidate_commit does not match the commit being qualified')
    if not isinstance(ledger.get('candidate_version'), str) or not ledger['candidate_version'].strip():
        problems.append('candidate_version is required')
    gates = ledger.get('gates', {})
    if not isinstance(gates, dict):
        return problems + ['gates must be an object']
    for key in RC_GATES if phase == 'rc' else STABLE_GATES:
        gate = gates.get(key)
        if not isinstance(gate, dict) or gate.get('status') != 'passed':
            problems.append(f'{key}: missing or not passed')
            continue
        reviewer = gate.get('reviewer')
        if not isinstance(reviewer, str) or not reviewer.strip():
            problems.append(f'{key}: named reviewer required')
        evidence = gate.get('evidence')
        if not isinstance(evidence, list) or not evidence:
            problems.append(f'{key}: evidence files required')
            continue
        for item in evidence:
            try:
                if not isinstance(item, dict):
                    raise ValueError('evidence item must be an object')
                path = (root / item['path']).resolve()
                if not path.is_relative_to(root.resolve()):
                    raise ValueError('evidence must be inside the ledger directory')
                # Stream reports/artifacts so binary evidence does not consume RAM.
                digest = hashlib.sha256()
                with path.open('rb') as source:
                    for block in iter(lambda: source.read(1024 * 1024), b''):
                        digest.update(block)
                if digest.hexdigest() != item['sha256']:
                    raise ValueError('SHA-256 mismatch')
            except (OSError, KeyError, TypeError, ValueError) as error:
                problems.append(f'{key}: invalid evidence: {error}')
        minimum = {'workload-soak': 24, 'tester-feedback': 5 * 24}.get(key)
        if minimum is not None:
            try:
                elapsed = (timestamp(gate['ended_at']) - timestamp(gate['started_at'])).total_seconds()
                if elapsed < minimum * 3600:
                    raise ValueError(f'at least {minimum} hours required')
                if timestamp(gate['ended_at']) > datetime.now().astimezone():
                    raise ValueError('end time is in the future')
            except (KeyError, TypeError, ValueError) as error:
                problems.append(f'{key}: invalid observation window: {error}')
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('ledger', type=Path)
    parser.add_argument('--init', action='store_true', help='create an untested ledger without overwriting')
    parser.add_argument('--phase', choices=['rc', 'stable'], default='rc')
    parser.add_argument('--commit', help='exact 40-character candidate source commit')
    args = parser.parse_args()
    if args.init:
        args.ledger.parent.mkdir(parents=True, exist_ok=True)
        with args.ledger.open('x') as output:
            json.dump(template(), output, indent=2)
            output.write('\n')
        print(f'Created untested ledger: {args.ledger}')
        return 0
    if not args.commit or len(args.commit) != 40 or any(c not in '0123456789abcdef' for c in args.commit):
        parser.error('--commit must be a full lowercase Git commit ID')
    try:
        ledger = json.loads(args.ledger.read_text())
        if not isinstance(ledger, dict):
            raise ValueError('ledger must be an object')
        problems = check(ledger, args.ledger.parent, args.phase, args.commit)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    for problem in problems:
        print(f'BLOCKED: {problem}')
    if problems:
        return 1
    print(f'{args.phase} evidence ledger complete; human review of evidence is still required. No release action performed.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
