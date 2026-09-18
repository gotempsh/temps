// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Card, CardContent, CardHeader, CardTitle } from '@temps-sdk/ui'
import { CopyAction, Detail, Kbd, Status, TimeChart, fmtDate, fmtDuration } from '@temps-sdk/ds'
import { DEPLOYMENT_METRICS, DEPLOYMENTS } from '../fixtures'

/** Reference screen for the `Detail` template's record recipe. */
export default function DeploymentDetail() {
  const deployment = DEPLOYMENTS[0]

  return (
    <Detail
      title={`${deployment.service} · ${deployment.branch}`}
      description="Deployment detail"
      verdict={<Status tone={deployment.status} label={deployment.statusLabel} />}
      actions={
        <span className="inline-flex items-center gap-1 rounded-md border border-dashed bg-muted/40 py-1 pl-2 pr-1 font-mono text-sm">
          {deployment.id}
          <CopyAction value={deployment.id} label="Copy deployment id" />
        </span>
      }
      facts={[
        { label: 'Commit', value: <span className="font-mono">{deployment.commit}</span> },
        { label: 'Author', value: deployment.author },
        { label: 'Duration', value: fmtDuration(deployment.durationMs) },
        { label: 'Deployed', value: fmtDate(deployment.createdAt) },
        { label: 'Region', value: 'eu-west' },
        { label: 'Instance', value: 'shared-cpu-1x' },
      ]}
      main={
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Response latency (last 24h)</CardTitle>
          </CardHeader>
          <CardContent>
            <TimeChart
              data={DEPLOYMENT_METRICS}
              xKey="t"
              series={[
                { dataKey: 'p50', label: 'p50' },
                { dataKey: 'p99', label: 'p99', tone: 'warn' },
              ]}
              height={260}
              yTickFormatter={(v) => `${v}ms`}
            />
          </CardContent>
        </Card>
      }
      aside={
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Shortcuts</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm text-muted-foreground">
            <div className="flex items-center justify-between">
              <span>Redeploy</span>
              <Kbd keys={['⌘', 'R']} showOnMobile />
            </div>
            <div className="flex items-center justify-between">
              <span>View logs</span>
              <Kbd keys={['⌘', 'L']} showOnMobile />
            </div>
          </CardContent>
        </Card>
      }
    />
  )
}
