// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { AlertTriangle, CircleDollarSign, Cpu, HardDrive, Network, Rocket } from 'lucide-react'
import {
  Article,
  Button,
  Callout,
  CompactRow,
  CopyAction,
  DataTable,
  Field,
  FormErrors,
  Kbd,
  LogLine,
  notify,
  PageContainer,
  PageHeader,
  PageState,
  Picker,
  ProjectAvatar,
  ResourceStat,
  Status,
  STATUS_TONES,
  TimeChart,
  EchoDialog,
  fmtBytes,
  fmtDate,
  fmtDuration,
  fmtNumber,
  fmtPercent,
  fmtRelativeTime,
  type StatusTone,
} from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'
import { DEPLOYMENT_METRICS, DEPLOYMENTS, type DeploymentFixture } from '../fixtures'

function Block({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-3 rounded-lg border p-6">
      <h2 className="text-lg font-semibold">{title}</h2>
      <div className="flex flex-wrap items-start gap-4">{children}</div>
    </section>
  )
}

export default function Components() {
  const [busy, setBusy] = useState(false)
  const [echoOpen, setEchoOpen] = useState(false)
  const [picked, setPicked] = useState<string>()

  return (
    <PageContainer>
      <PageHeader title="Components" description="One block per primitive, with a working demo." />

      <Block title="PageHeader / PageContainer">
        <div className="w-full rounded-md border p-4">
          <PageHeader
            title="Example page"
            description="Description text goes here."
            verdict={<Status tone="ok" />}
            actions={<Button>Primary action</Button>}
          />
        </div>
      </Block>

      <Block title="Status">
        {(Object.keys(STATUS_TONES) as StatusTone[]).map((tone) => (
          <Status key={tone} tone={tone} />
        ))}
        {(Object.keys(STATUS_TONES) as StatusTone[]).map((tone) => (
          <Status key={`${tone}-dot`} tone={tone} variant="dot" />
        ))}
      </Block>

      <Block title="Kbd">
        <Kbd keys="N" showOnMobile />
        <Kbd keys={['⌘', 'K']} showOnMobile />
      </Block>

      <Block title="LogLine">
        <div className="w-full rounded-md border bg-background">
          <LogLine content="Starting build for checkout-api@a1b2c3d" />
          <LogLine content="Installing dependencies..." isHighlighted />
          <LogLine content="Build failed: timeout waiting for database connection" searchTerm="timeout" />
        </div>
      </Block>

      <Block title="CompactRow">
        <div className="w-full rounded-md border">
          <CompactRow
            timestamp={new Date(Date.now() - 3 * 60_000).toISOString()}
            icon={<Network className="h-3.5 w-3.5" />}
            primary={
              <>
                <span className="font-mono text-xs">POST</span>{' '}
                <span>/api/checkout/session</span>
              </>
            }
            secondary="checkout-api.internal"
            meta={<span className="font-mono tabular-nums">214ms</span>}
          />
          <CompactRow
            timestamp={new Date(Date.now() - 41 * 60_000).toISOString()}
            icon={<CircleDollarSign className="h-3.5 w-3.5 text-emerald-500" />}
            primary="checkout.completed"
            meta={<span className="font-mono tabular-nums text-emerald-600">$48.00</span>}
          />
        </div>
      </Block>

      <Block title="ResourceStat">
        <ResourceStat icon={Cpu} value={fmtPercent(0.42)} />
        <ResourceStat icon={HardDrive} value={fmtBytes(6_710_886_400)} limit={` / ${fmtPercent(0.71)}`} />
      </Block>

      <Block title="Button (busy)">
        <Button
          busy={busy}
          busyLabel="Deploying…"
          onClick={() => {
            setBusy(true)
            setTimeout(() => setBusy(false), 1500)
          }}
        >
          <Rocket /> Deploy
        </Button>
      </Block>

      <Block title="CopyAction">
        <span className="inline-flex items-center gap-1 rounded-md border border-dashed bg-muted/40 py-1 pl-2 pr-1 font-mono text-sm">
          tck_a1b2c3d4e5f6
          <CopyAction value="tck_a1b2c3d4e5f6" label="Copy API key" />
        </span>
      </Block>

      <Block title="notify">
        <Button variant="secondary" onClick={() => notify.ok('Deployment live')}>
          Trigger notify.ok
        </Button>
        <Button
          variant="secondary"
          onClick={() => notify.fail('Deployment failed', 'Build exited with code 1.')}
        >
          Trigger notify.fail
        </Button>
      </Block>

      <Block title="Field / FormErrors">
        <div className="w-full max-w-sm space-y-4">
          <FormErrors errors={{ name: 'Project name is required.' }} />
          <Field label="Project name" error="Project name is required.">
            {(props) => <Input {...props} />}
          </Field>
        </div>
      </Block>

      <Block title="Callout">
        <div className="w-full space-y-2">
          <Callout tone="info">Informational notice.</Callout>
          <Callout tone="success">Everything's fine.</Callout>
          <Callout tone="warning">Approaching a limit.</Callout>
          <Callout tone="error">Something failed.</Callout>
        </div>
      </Block>

      <Block title="PageState">
        <div className="grid w-full gap-4 md:grid-cols-3">
          <PageState
            variant="empty"
            size="compact"
            icon={Rocket}
            title="No deployments yet"
            description="Push to your connected branch to trigger the first one."
          />
          <PageState
            variant="not-set-up"
            size="compact"
            icon={AlertTriangle}
            title="AI error triage isn't configured"
            requirement="No AI provider configured for this project."
            example="Once configured, a failing deploy would get a one-line root-cause summary here."
            settingsHref="/settings/ai"
            settingsLabel="Open AI settings"
          />
          <PageState
            variant="failed"
            size="compact"
            icon={AlertTriangle}
            title="Couldn't load deployments"
            description="The request timed out. Try again."
          />
        </div>
      </Block>

      <Block title="Article">
        <Article className="max-w-none">
          <p>
            Long-form content — release notes, postmortems, docs — read top
            to bottom rather than scanned for facts. Built on Tailwind
            Typography, restyled to stay inside the token system.
          </p>
          <h3>A heading</h3>
          <p>
            Body copy, <code>inline code</code>, and{' '}
            <a href="/guide">a link</a> all pick up token colors instead of
            the plugin's defaults.
          </p>
        </Article>
      </Block>

      <Block title="EchoDialog">
        <Button variant="destructive" onClick={() => setEchoOpen(true)}>
          Delete project
        </Button>
        <EchoDialog
          open={echoOpen}
          onOpenChange={setEchoOpen}
          title="Delete checkout-api?"
          description="This permanently deletes the project, its deployments, and its logs."
          phrase="checkout-api"
          confirmLabel="Delete project"
          onConfirm={() => setEchoOpen(false)}
        />
      </Block>

      <Block title="Picker">
        <Picker
          className="w-full max-w-sm"
          items={[
            {
              value: 'checkout-api',
              label: 'checkout-api',
              icon: <ProjectAvatar name="checkout-api" className="size-5" />,
            },
            {
              value: 'marketing-site',
              label: 'marketing-site',
              icon: <ProjectAvatar name="marketing-site" className="size-5" />,
            },
            {
              value: 'worker-pool',
              label: 'worker-pool',
              icon: <ProjectAvatar name="worker-pool" className="size-5" />,
            },
          ]}
          value={picked}
          onValueChange={setPicked}
        />
      </Block>

      <Block title="DataTable">
        <p className="w-full text-sm text-muted-foreground">
          The table + skeleton + pagination footer `Ledger` composes — usable
          standalone for an embedded table (a settings sub-panel, a
          `Detail`'s `main` column) that doesn't want a second page header.
        </p>
        <div className="w-full">
          <DataTable<DeploymentFixture>
            columns={[
              { key: 'service', header: 'Service', render: (d) => <span className="font-medium">{d.service}</span> },
              { key: 'status', header: 'Status', render: (d) => <Status tone={d.status} label={d.statusLabel} /> },
              { key: 'duration', header: 'Duration', render: (d) => fmtDuration(d.durationMs) },
            ]}
            rows={DEPLOYMENTS.slice(0, 3)}
            rowKey={(d) => d.id}
          />
        </div>
      </Block>

      <Block title="CardGrid">
        <p className="w-full text-sm text-muted-foreground">
          A responsive grid shell for record collections better shown as
          cards than a table — same header shape as `Ledger`
          (title/description/actions/toolbar), but a full template (owns its
          own `PageHeader`), so it isn't demoed inline here — see the
          "CardGrid — Projects" reference screen for a working example with
          filtering, empty state and a card renderer.
        </p>
      </Block>

      <Block title="TimeChart">
        <div className="w-full">
          <TimeChart
            data={DEPLOYMENT_METRICS}
            xKey="t"
            series={[{ dataKey: 'p50', label: 'p50' }]}
            height={200}
            yTickFormatter={(v) => `${v}ms`}
          />
        </div>
      </Block>

      <Block title="fmt.ts">
        <ul className="w-full space-y-1 text-sm">
          <li>fmtNumber(1234567) → {fmtNumber(1234567)}</li>
          <li>fmtPercent(0.834) → {fmtPercent(0.834)}</li>
          <li>fmtBytes(15_728_640) → {fmtBytes(15_728_640)}</li>
          <li>fmtDuration(1834) → {fmtDuration(1834)}</li>
          <li>fmtRelativeTime(-3600s) → {fmtRelativeTime(Date.now() - 3_600_000)}</li>
          <li>fmtDate(now) → {fmtDate(Date.now())}</li>
        </ul>
      </Block>
    </PageContainer>
  )
}
