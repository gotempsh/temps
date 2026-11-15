# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Official Python 2.42.1 logs, ae337ca4b54b30621778c0847b93605e57e08314.

Synthetic int64 values deliberately exceed JavaScript's exact numeric range.
Use SENTRY_DSN and a synthetic run key argument; no credentials are printed.
"""
import json
import logging
import os
import sys
import sentry_sdk
from sentry_sdk.integrations.logging import LoggingIntegration

assert sentry_sdk.VERSION == '2.42.1'


def sanitize(log, hint=None):
    attributes = log['attributes']
    for key in list(attributes):
        if key.startswith('code.file') or key in ('code.filename', 'code.filepath'):
            attributes[key] = 'synthetic/application.py'
    attributes['server.address'] = 'synthetic.example'
    return log


sentry_sdk.init(dsn=os.environ['SENTRY_DSN'], enable_logs=True,
                default_integrations=False, send_client_reports=False,
                integrations=[LoggingIntegration(level=None, event_level=None, sentry_logs_level=logging.INFO)],
                before_send_log=sanitize, traces_sample_rate=1,
                server_name='synthetic.example', release='synthetic-typed-release', environment='synthetic-test')
prefix = sys.argv[1]
logger = logging.getLogger('synthetic.typed')
logger.setLevel(logging.INFO)
logger.propagate = False
with sentry_sdk.start_transaction(name='Synthetic typed log trace', op='task') as transaction:
    trace_id = transaction.trace_id
    for label, count, flag in [('integer', 9007199254740993, True),
                               ('string', '9007199254740993', 'true'),
                               ('neighbor', 9007199254740992, False)]:
        logger.info('Synthetic typed log %s', label,
                    extra={'synthetic.run': prefix, 'count': count, 'flag': flag, 'fraction': 0.125})
sentry_sdk.flush(timeout=10)
sentry_sdk.get_client().close(timeout=10)
print(json.dumps({'sdk': sentry_sdk.VERSION, 'trace_id': trace_id, 'logs': 3}))
