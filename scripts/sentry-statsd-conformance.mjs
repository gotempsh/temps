// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official @sentry/node 8.47.0, eac1a8821988618eb12821011be901a12958978f.
// Exercise its legacy metrics aggregator, without constructing wire fixtures.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const [entry, run] = process.argv.slice(2);
const S = await import(pathToFileURL(entry).href);
assert.equal(S.SDK_VERSION, '8.47.0');
const receipts=[];
S.init({ dsn:process.env.SENTRY_DSN,defaultIntegrations:false,sendClientReports:false,
  release:'synthetic-statsd-release',environment:'synthetic-test',serverName:'synthetic.example',
  transport(options) {const native=S.makeNodeTransport(options);return {
    send: envelope=>native.send(envelope).then(result=>{receipts.push(result.statusCode);return result;}),
    flush: timeout=>native.flush(timeout),
  };},
});
const tags={'synthetic.run':run,'synthetic.escaped':'comma,pipe|newline\nend'};
const timestamp=Math.floor(Date.now()/1000);
S.metrics.increment('synthetic.legacy.requests',3,{tags,timestamp});
S.metrics.increment('synthetic.legacy.requests',-1,{tags,timestamp});
S.metrics.gauge('synthetic.legacy.queue',2,{tags,timestamp});
S.metrics.gauge('synthetic.legacy.queue',7,{tags,timestamp});
S.metrics.distribution('synthetic.legacy.duration',10,{unit:'millisecond',tags,timestamp});
S.metrics.distribution('synthetic.legacy.duration',30,{unit:'millisecond',tags,timestamp});
S.metrics.set('synthetic.legacy.users','synthetic-user',{tags,timestamp});
assert.ok(await S.flush(10000));await S.close(10000);
assert.ok(receipts.length>0 && receipts.every(code=>code===200));
console.log(JSON.stringify({sdk:S.SDK_VERSION,deliveries:receipts.length,timestamp}));
