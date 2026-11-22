# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Verify official SDK feedback through a dedicated local API and native inbox.

Run with --api, --project, --email, --password-file, repeated --node-entry,
and optional --browser-entry. Uses the same SDK pins as core conformance.
Never prints credentials, DSNs, payloads or unfiltered SDK process failures.
"""
import argparse
import http.cookiejar
import json
from pathlib import Path
import subprocess
import urllib.error
import urllib.parse
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--api', required=True)
parser.add_argument('--project', required=True, type=int)
parser.add_argument('--email', required=True)
parser.add_argument('--password-file', required=True, type=Path)
parser.add_argument('--node-entry', action='append', default=[])
parser.add_argument('--browser-entry')
parser.add_argument('--legacy-entry')
args = parser.parse_args()
if urllib.parse.urlsplit(args.api).hostname not in ('localhost', '127.0.0.1', '::1'):
    parser.error('Use a dedicated local synthetic instance')
base = args.api.rstrip('/') + '/api'
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))


def request(path, data=None, method=None):
    body = None if data is None else json.dumps(data).encode()
    with opener.open(urllib.request.Request(base + path, data=body, method=method,
                     headers={'Content-Type': 'application/json'}), timeout=30) as response:
        return json.load(response) if response.status != 204 else None


request('/auth/login', {'email': args.email, 'password': args.password_file.read_text().strip()})
dsn = request(f'/projects/{args.project}/dsns/get-or-create', {'base_url': args.api})['dsn']
fixture = str(Path(__file__).with_suffix('.mjs'))
cases = [('node', 'node', entry) for entry in args.node_entry]
if args.browser_entry:
    cases.append(('browser', 'bun', args.browser_entry))
if args.legacy_entry:
    cases.append(('legacy', 'bun', args.legacy_entry))
assert cases, 'Supply at least one pinned SDK entry'
for name, runtime, entry in cases:
    for tunnel in (False, True):
        prefix = 'synthetic-feedback-' + uuid.uuid4().hex
        script = str(Path(__file__).with_name('sentry-feedback-legacy-conformance.mjs')) if name == 'legacy' else fixture
        command = [runtime, script, entry, dsn, prefix]
        if tunnel:
            command += [base + '/_temps/sentry/envelope']
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired:
            raise AssertionError(f'{name} SDK timed out (command and output suppressed)') from None
        assert result.returncode == 0, f'{name} SDK failed (output suppressed)'
        emitted = json.loads(result.stdout.strip().splitlines()[-1])
        assert emitted['sdk'] in ('7.120.4', '8.47.0', '10.17.0', '11.8.0')
        rows = request(f'/projects/{args.project}/feedback?page_size=100')['items']
        reports = [row for row in rows if row['message'].endswith(prefix)]
        assert len(reports) == 2, 'Native inbox must contain exactly two reports after retry'
        legacy = name == 'legacy'
        linked = next(row for row in reports if 'linked report' in row['message'])
        standalone = next(row for row in reports if row['id'] != linked['id'])
        assert linked['associated_event_id'] == (emitted['ids'][0] if legacy else emitted['error_id'])
        assert linked['error_group_id'], 'Native error linkage is missing'
        assert linked['message'] == ('Synthetic legacy linked report ' if legacy else 'Synthetic linked report ') + prefix
        assert linked['email'] == 'synthetic@example.invalid'
        assert linked['name'] == 'Synthetic user'
        if not legacy:
            assert linked['source_id'] in emitted['ids']
            assert linked['release'] == 'synthetic-feedback-release'
            assert linked['context']['tags']['synthetic.run'] == prefix
            assert linked['context']['contexts']['synthetic']['count'] == 7
        assert standalone['message'] == ('Synthetic legacy second report ' if legacy else 'Synthetic standalone report ') + prefix
        if name == 'browser':
            assert linked['replay_id'] == emitted['replay_id']
            assert linked['replay_session_id'] and linked['replay_visitor_id'], 'Native replay linkage is missing'
            logs = request(f'/otel/logs?project_id={args.project}&trace_id={emitted["trace_id"]}')['data']
            assert len(logs) == 1 and logs[0]['span_id'], 'Native correlated browser log is missing'
            assert logs[0]['attributes']['user.ip_address'] in ('127.0.0.1', '::1')
            assert logs[0]['attributes']['browser.name'], 'Default browser log UA inference is missing'
            spans = request(f'/otel/traces/{args.project}/{emitted["trace_id"]}')['data']
            assert len(spans) == 2
            assert all(span['attributes'].get('browser.name') for span in spans), 'Default browser span UA inference is missing'
            for metric, expected in [('requests', 1), ('queue', 7), ('duration', 12.5)]:
                query = urllib.parse.urlencode({'project_id': args.project, 'metric_name': 'synthetic.feedback.' + metric,
                                               'label_filters': 'synthetic.run=' + prefix,
                                               'group_by': 'user.ip_address,browser.name'})
                buckets = request('/otel/metrics?' + query)['data']
                assert len(buckets) == 1 and buckets[0]['value'] == expected, 'Native metric value or inference is missing'
                labels = dict(buckets[0]['series_key'])
                assert labels['user.ip_address'] in ('127.0.0.1', '::1') and labels['browser.name']
            recording = request(f'/visitors/{linked["replay_visitor_id"]}/session-replays/{linked["replay_session_id"]}/events')
            events = recording['events']
            assert len(events) > 10 and any(event['event_type'] == 2 for event in events), 'Native replay snapshot is missing'
        path = f'/projects/{args.project}/feedback/{linked["id"]}'
        request(path, {'status': 'resolved'}, 'PATCH')
        assert request(path)['status'] == 'resolved'
        request(path, {'status': 'open'}, 'PATCH')
        assert request(path)['status'] == 'open'
        # A project path must never grant access to another project's record.
        try:
            request(f'/projects/2147483647/feedback/{linked["id"]}')
            raise AssertionError('Feedback leaked across projects')
        except urllib.error.HTTPError as error:
            assert error.code in (403, 404)
        delete_path = f'/projects/{args.project}/feedback/{standalone["id"]}'
        request(delete_path, method='DELETE')
        try:
            request(delete_path)
            raise AssertionError('Deleted native report is still queryable')
        except urllib.error.HTTPError as error:
            assert error.code == 404
        print(json.dumps({'sdk': emitted['sdk'], 'runtime': name, 'tunnel': tunnel,
                          'stored': 2, 'retry_deduplication': True, 'native_error_link': True,
                          'native_replay_link': name == 'browser', 'native_triage': True, 'project_isolation': True, 'deletion': True}), flush=True)
