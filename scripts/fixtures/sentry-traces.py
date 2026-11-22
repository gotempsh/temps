# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Synthetic sentry-sdk 2.42.1 traces, ae337ca4b54b30621778c0847b93605e57e08314."""
import json
import os
import sys
import time
import sentry_sdk

assert sentry_sdk.VERSION == '2.42.1'
sentry_sdk.init(dsn=os.environ['SENTRY_DSN'], default_integrations=False,
                send_client_reports=False, traces_sample_rate=1,
                release='synthetic-tracing-release', environment='synthetic-test', server_name='synthetic.example')
prefix = sys.argv[1]
with sentry_sdk.start_transaction(name='Synthetic cross SDK root', op='http.server') as root:
    trace_id = root.trace_id
    root_id = root.span_id
    root.set_data('service.name', 'synthetic-tracing-service')
    root.set_data('sentry.kind', 2)
    root.set_tag('synthetic.run', prefix)
    root.set_status('ok')
    with root.start_child(op='db.query', description='Synthetic cross SDK child') as child:
        child_id = child.span_id
        child.set_data('synthetic.flag', True)
        child.set_data('sentry.kind', 3)
        child.set_tag('synthetic.run', prefix)
        child.set_status('internal_error')
        time.sleep(0.002)
sentry_sdk.flush(timeout=10)
sentry_sdk.get_client().close(timeout=10)
print(json.dumps({'sdk': sentry_sdk.VERSION, 'trace_id': trace_id, 'root_id': root_id, 'child_id': child_id}))
