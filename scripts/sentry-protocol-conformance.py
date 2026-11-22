# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Pinned CLI/Node SDK -> real endpoints -> native Debug ID and client outcome queries."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import http.cookiejar
import json
import os
import re
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--api', required=True)
parser.add_argument('--project', type=int, required=True)
parser.add_argument('--slug', required=True)
parser.add_argument('--email', required=True)
parser.add_argument('--password-file', type=Path, required=True)
parser.add_argument('--node-entry', required=True)
parser.add_argument('--sentry-cli', required=True)
parser.add_argument('--esbuild-entry', required=True)
parser.add_argument('--stage', choices=('all','debug','outcomes','browser'), default='all')
args = parser.parse_args()
if urllib.parse.urlsplit(args.api).hostname not in ('localhost', '127.0.0.1', '::1'):
    parser.error('Use a dedicated local synthetic instance')
base = args.api.rstrip('/') + '/api'
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))


def request(path, data=None, method=None):
    with opener.open(urllib.request.Request(base + path, data=None if data is None else json.dumps(data).encode(),
                     method=method, headers={'Content-Type': 'application/json'}), timeout=30) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def invoke(runtime, name, argv, env):
    result = subprocess.run([runtime, str(Path(__file__).with_name(name)), *argv], env={**os.environ, **env},
                            capture_output=True, text=True, timeout=180)
    # Native auth and SDK/tool failures can contain credentials: never echo them.
    stages = re.findall(r'SYNTHETIC_STAGE=([a-z_]+)', result.stderr)
    hint = ' stage=' + stages[-1] if stages else ''
    diagnostic = re.search(r'SYNTHETIC_DIAGNOSTIC=([^\n]+)', result.stderr)
    if diagnostic:
        fields = json.loads(diagnostic.group(1))
        hint += ' diagnostics=' + json.dumps({key: value for key, value in fields.items()
            if key in ('frames', 'injected_ids', 'expected_injected', 'images') and isinstance(value, (int, bool))})
    failure = re.search(r'SYNTHETIC_FAILURE=([^\n]+)', result.stderr)
    if failure:
        hint += ' failure=' + failure.group(1)[:250]
    assert result.returncode == 0, f'{name} failed (output suppressed)' + hint
    return json.loads(result.stdout.strip().splitlines()[-1])


def find_event(event_id, run):
    groups = request(f'/projects/{args.project}/error-groups?page_size=100&sort_by=last_seen&sort_order=desc')['data']
    for group in groups:
        query = urllib.parse.urlencode({'page_size': 100, 'tag_key': 'synthetic.run', 'tag_value': run})
        events = request(f'/projects/{args.project}/error-groups/{group["id"]}/events?{query}')['data']
        for event in events:
            if event['data']['sentry'].get('event_id') == event_id:
                return request(f'/projects/{args.project}/error-groups/{group["id"]}/events/{event["id"]}')
    raise AssertionError('Official SDK event missing from native queries')


request('/auth/login', {'email': args.email, 'password': args.password_file.read_text().strip()})
dsn = request(f'/projects/{args.project}/dsns/get-or-create', {'base_url': args.api})['dsn']
if args.stage in ('all', 'debug'):
    credential = request(f'/projects/{args.project}/deployment-tokens',
                         {'name': 'synthetic-artifacts-' + uuid.uuid4().hex, 'permissions': ['*']})
    try:
        run = 'synthetic-debug-' + uuid.uuid4().hex
        debug = invoke('node', 'sentry-debug-id-conformance.mjs', [args.node_entry, args.sentry_cli, run, args.esbuild_entry], {
            'SENTRY_DSN': dsn, 'SENTRY_AUTH_TOKEN': credential['token'], 'SENTRY_URL': args.api,
            'SYNTHETIC_PROJECT_SLUG': args.slug, 'SENTRY_RELEASE': '',
        })
        event = find_event(debug['event_id'], run)
        raw = event['data']['sentry']
        assert not raw.get('release')
        frames = [frame for exception in raw['exception']['values'] for frame in exception.get('stacktrace', {}).get('frames', [])]
        frame = next((frame for frame in frames if frame.get('symbolicated') and frame.get('debug_id') == debug['debug_id']), None)
        assert frame is not None, 'Debug ID upload did not symbolicate native error detail'
        assert 'synthetic-original.ts' in frame['filename']
        assert 'throw new Error' in frame.get('context_line', '')
        assert frame.get('original_filename') and frame.get('original_lineno')
        assert any('synthetic-original.ts' in row.get('filename', '') and row.get('context_line')
                   for row in event['data'].get('stack_trace', [])), 'Native stack normalization did not receive resolved frames'
        maps = request(f'/projects/{args.project}/releases/' + urllib.parse.quote('debug-id:' + debug['debug_id'], safe='') + '/source-maps')
        assert maps['total'] == 1 and maps['source_maps'][0]['debug_id'] == debug['debug_id']
        # The public ingestion key cannot read or upload build artifacts.
        key = urllib.parse.urlsplit(dsn).username
        for path in ['/0/organizations/temps/chunk-upload/', f'/projects/{args.project}/source-map-releases']:
            try:
                urllib.request.urlopen(urllib.request.Request(base + path, headers={'Authorization': 'Bearer ' + key}), timeout=30)
                raise AssertionError('Public DSN accessed source artifacts')
            except urllib.error.HTTPError as error:
                assert error.code in (401, 403)
        print(json.dumps({'debug_id': True, 'cli': debug['cli'], 'sdk': debug['sdk'], 'native_source_maps': maps['total'],
                          'late_symbolication': True, 'repeat_upload': True, 'public_dsn_write_only': True}), flush=True)
    finally:
        request(f'/projects/{args.project}/deployment-tokens/{credential["id"]}', method='DELETE')


def totals():
    found = {}
    for number in range(1, 101):
        page = request(f'/projects/{args.project}/client-outcomes?page={number}&page_size=100&days=1')
        assert page['approximate'] is True
        assert page['total'] <= 10_000, 'Use an isolated synthetic project with at most 10,000 outcome rows'
        for row in page['items']:
            key = row['reason'] + '/' + row['category']
            found[key] = found.get(key, 0) + int(row['quantity'])
        if number * 100 >= page['total']:
            break
    return found


if args.stage in ('all', 'outcomes'):
    before = totals()
    run = 'synthetic-outcomes-' + uuid.uuid4().hex
    outcomes = invoke('node', 'sentry-outcomes-conformance.mjs', [args.node_entry, run], {'SENTRY_DSN': dsn})
    after = totals()
    for key, quantity in outcomes['totals'].items():
        assert after.get(key, 0) - before.get(key, 0) == quantity, f'Native outcome quantity mismatch for {key}'
    assert find_event(outcomes['event_id'], run)
    assert outcomes['independent_clients'] == 2 and outcomes['stable_report_identity'] is False
    # Wire-equivalent independent reports may even have the same timestamp.
    # No identity is required: content-based deduplication would lose valid
    # counts. This explicit fixture supplements the actual SDK clients above.
    payload = json.dumps({'timestamp': int(time.time()), 'discarded_events': [
        {'reason': 'sample_rate', 'category': 'span', 'quantity': 7}]}).encode()
    envelope = b'{}\n' + json.dumps({'type': 'client_report', 'length': len(payload)}).encode() + b'\n' + payload
    target = args.api.rstrip('/') + f'/api/{args.project}/envelope/?' + urllib.parse.urlencode({
        'sentry_key': urllib.parse.urlsplit(dsn).username})
    def deliver_outcomes(body):
        with urllib.request.urlopen(urllib.request.Request(target, data=body,
                headers={'Content-Type': 'application/x-sentry-envelope'}), timeout=30) as response:
            assert response.status == 200
            return (int(response.headers['x-temps-sentry-client-outcomes-accepted']),
                    int(response.headers['x-temps-sentry-client-outcomes-duplicates']))
    for _ in range(2):
        assert deliver_outcomes(envelope) == (1, 0)
    assert totals().get('sample_rate/span', 0) - after.get('sample_rate/span', 0) == 14
    # Identified-context fixtures use only existing Sentry envelope fields. The
    # event ID comes from the actual official SDK companion event above; no
    # custom transport/SDK identity extension is required by the receiver.
    item = json.dumps({'type': 'client_report', 'length': len(payload)}).encode() + b'\n' + payload + b'\n'
    header = json.dumps({'event_id': outcomes['event_id']}).encode() + b'\n'
    identified = header + item
    with ThreadPoolExecutor(max_workers=2) as executor:
        receipts = list(executor.map(deliver_outcomes, [identified] * 12))
    assert sum(receipt[0] for receipt in receipts) == 1
    assert sum(receipt[1] for receipt in receipts) == 11
    assert deliver_outcomes(json.dumps({'event_id': uuid.uuid4().hex}).encode() + b'\n' + item) == (1, 0)
    assert deliver_outcomes(header + item + item) == (1, 1)
    # An updated report under the same event ID is new information, not a retry.
    changed = json.loads(payload)
    changed['discarded_events'][0]['quantity'] = 11
    changed = json.dumps(changed).encode()
    assert deliver_outcomes(header + json.dumps({'type': 'client_report', 'length': len(changed)}).encode()
                            + b'\n' + changed) == (1, 0)
    assert totals().get('sample_rate/span', 0) - after.get('sample_rate/span', 0) == 46
    print(json.dumps({'client_outcomes': True, 'sdk': outcomes['sdk'], 'native_quantities': True,
                      'filtered_errors': 3, 'filtered_logs': 1, 'bytes_and_items_separate': True,
                      'independent_sdk_clients': 2, 'identical_unidentified_reports_preserved': True,
                      'identified_concurrent_requests': 12, 'identified_retry_duplicates': 11,
                      'independent_event_ids_and_report_positions': True, 'updated_report_preserved': True,
                      'approximate_by_protocol': True}), flush=True)

if args.stage in ('all', 'browser'):
    run = 'synthetic-browser-reports-' + uuid.uuid4().hex
    reports = invoke('node', 'sentry-browser-reports-conformance.mjs', [args.node_entry, run], {'SENTRY_DSN': dsn})
    groups = request(f'/projects/{args.project}/error-groups?page_size=100&sort_by=last_seen&sort_order=desc')['data']
    csp_found = False
    for group in groups:
        if group['error_type'] != 'CSPViolation':
            continue
        events = request(f'/projects/{args.project}/error-groups/{group["id"]}/events?page_size=100')['data']
        for event in events:
            raw = event['data']['sentry']
            if raw.get('release') == run and event['data']['request'].get('url') == reports['source_url']:
                assert raw['csp']['blocked_uri'] == 'inline'
                assert raw['csp']['effective_directive'].startswith('script-src')
                assert raw['temps.security_report']['csp-report']['document-uri'] == reports['source_url']
                csp_found = True
    assert csp_found, 'Browser-generated CSP report did not reach native issues'
    query = urllib.parse.urlencode({'project_id': args.project, 'service_name': 'sentry.nel',
                                   'attribute_key': 'url.full', 'attribute_value': 'https://synthetic.example/' + run,
                                   'attribute_type': 'string', 'limit': 100})
    logs = request('/otel/logs?' + query)['data']
    assert len(logs) == 1
    log = logs[0]
    assert log['attributes']['nel.type'] == 'dns.name_not_resolved'
    assert log['attribute_types']['http.response.status_code'] == 'integer'
    assert log['resource']['service_version'] == run
    assert log['trace_id'] is None and log['severity'] == 'WARN'
    assert reports['automatic_nel'] and reports['automatic_reports'] > 0
    query = urllib.parse.urlencode({'project_id': args.project, 'service_name': 'sentry.nel',
                                   'attribute_key': 'url.full', 'attribute_value': reports['automatic_source_url'],
                                   'attribute_type': 'string', 'limit': 100})
    automatic = request('/otel/logs?' + query)['data']
    assert len(automatic) == reports['automatic_reports'], 'Browser-generated NEL report missing from native logs'
    for log in automatic:
        assert log['attributes']['nel.type'] == 'http.error'
        assert log['attributes']['nel.phase'] == 'application'
        assert log['attributes']['http.response.status_code'] == '503'
        assert log['attribute_types']['http.response.status_code'] == 'integer'
        assert log['attributes']['http.request.method'] == 'GET'
        assert log['resource']['service_version'] == run
        assert log['trace_id'] is None and log['severity'] == 'WARN'
    print(json.dumps({'browser': reports['browser'], 'browser_csp_native_issue': True, 'nel_native_typed_log': True,
                      'automatic_browser_nel': reports['automatic_nel'], 'automatic_reports': reports['automatic_reports']}), flush=True)
