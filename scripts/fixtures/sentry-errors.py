# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Synthetic traffic from sentry-sdk==2.42.1 (ae337ca4b54b30621778c0847b93605e57e08314)."""
import json
import os
import sys
import sentry_sdk


def sanitize(event, hint):
    event['server_name'] = 'synthetic.example'
    for exception in event.get('exception', {}).get('values', []):
        for frame in exception.get('stacktrace', {}).get('frames', []):
            frame['filename'] = 'synthetic/application.py'
            frame['abs_path'] = '/synthetic/application.py'
            frame.pop('vars', None)
    return event


sentry_sdk.init(dsn=os.environ['SENTRY_DSN'], default_integrations=False,
                send_client_reports=False, include_local_variables=False,
                release='synthetic-release-2', environment='synthetic-test', before_send=sanitize)
prefix = sys.argv[1]
ids = []
with sentry_sdk.new_scope() as scope:
    scope.set_user({'id': 'synthetic-user', 'email': 'synthetic@example.invalid'})
    scope.set_tag('synthetic.run', prefix)
    scope.set_tag('synthetic.key', "test'value")
    scope.set_context('synthetic', {'nullable': None, 'array': [1, True, None]})
    sentry_sdk.add_breadcrumb(category='synthetic', message='Synthetic interaction', data={'count': 7})
    for index, fingerprint in enumerate(['shared', 'shared', 'split']):
        scope.fingerprint = [prefix, fingerprint]
        try:
            try:
                raise ValueError('Synthetic inner failure')
            except ValueError as cause:
                if index == 1:
                    raise TypeError('Synthetic different type') from cause
                raise RuntimeError('Synthetic outer failure') from cause
        except Exception:
            ids.append(sentry_sdk.capture_exception())
    scope.fingerprint = ['{{ default }}', prefix, 'message']
    ids.append(sentry_sdk.capture_message('Synthetic SDK message'))
    sentry_sdk.capture_event({'event_id': ids[0], 'message': 'Synthetic retry identity'})
sentry_sdk.flush(timeout=10)
assert len(ids) == 4 and all(ids)
print(json.dumps({'sdk': sentry_sdk.VERSION, 'ids': ids, 'retry': True}))
