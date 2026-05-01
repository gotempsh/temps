import { Fragment } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { ExternalLink } from 'lucide-react'
import { Link } from 'react-router-dom'
import type { ObservabilityEvent } from '../types'

/**
 * Side panel for any event kind. Renders entirely from the row payload —
 * no follow-up fetch in the common case. The "Show full" call lives on
 * each per-kind panel (see PanelStacktrace etc.) and only fires when the
 * user clicks it.
 */
export function ObservePanel({
  event,
  open,
  onOpenChange,
  projectSlug,
}: {
  event: ObservabilityEvent | null
  open: boolean
  onOpenChange: (open: boolean) => void
  projectSlug: string
}) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        className="w-full overflow-y-auto sm:max-w-xl lg:max-w-2xl"
      >
        {event && <PanelBody event={event} projectSlug={projectSlug} />}
      </SheetContent>
    </Sheet>
  )
}

function PanelBody({
  event,
  projectSlug,
}: {
  event: ObservabilityEvent
  projectSlug: string
}) {
  return (
    <>
      <SheetHeader className="space-y-2">
        <SheetTitle>{titleFor(event)}</SheetTitle>
        <SheetDescription className="font-mono text-xs">
          {new Date(event.ts).toISOString()}
        </SheetDescription>
      </SheetHeader>

      <div className="mt-6 space-y-6">
        <CorrelationStrip event={event} projectSlug={projectSlug} />
        <KindSpecificDetails event={event} projectSlug={projectSlug} />
      </div>
    </>
  )
}

function titleFor(event: ObservabilityEvent): string {
  switch (event.type) {
    case 'request':
      return `${event.method} ${event.path}`
    case 'span':
      return `${event.service} · ${event.operation}`
    case 'error':
      return event.error_class
    case 'revenue':
      return event.event_type
  }
}

/**
 * Universal cross-event correlation row: trace_id, deployment, environment.
 * Lets the user jump from any event to its peers in the same trace.
 */
function CorrelationStrip({
  event,
  projectSlug,
}: {
  event: ObservabilityEvent
  projectSlug: string
}) {
  const items: { label: string; value: React.ReactNode }[] = []
  if ('trace_id' in event && event.trace_id) {
    items.push({
      label: 'Trace',
      value: (
        <span className="flex items-center gap-1">
          <code className="font-mono text-xs">
            {event.trace_id.slice(0, 8)}…
          </code>
          <CopyButton value={event.trace_id} />
          <Link
            to={`/projects/${projectSlug}/traces/${event.trace_id}`}
            className="inline-flex items-center text-xs text-primary hover:underline"
          >
            <ExternalLink className="h-3 w-3" />
          </Link>
        </span>
      ),
    })
  }
  if (event.deployment_id != null) {
    items.push({
      label: 'Deployment',
      value: <code className="font-mono text-xs">#{event.deployment_id}</code>,
    })
  }
  if (event.environment_id != null) {
    items.push({
      label: 'Environment',
      value: <code className="font-mono text-xs">#{event.environment_id}</code>,
    })
  }
  if (items.length === 0) return null
  return (
    <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      {items.map((item) => (
        <Fragment key={item.label}>
          <span className="text-muted-foreground">{item.label}</span>
          <span>{item.value}</span>
        </Fragment>
      ))}
    </div>
  )
}

function KindSpecificDetails({
  event,
  projectSlug,
}: {
  event: ObservabilityEvent
  projectSlug: string
}) {
  switch (event.type) {
    case 'request':
      return (
        <>
          <Section title="Request">
            <KvList
              items={[
                ['Method', event.method],
                ['Host', event.host],
                ['Path', event.path],
                ['Query', event.query_string],
                ['Status', String(event.status)],
                [
                  'Latency',
                  event.latency_ms != null ? `${event.latency_ms}ms` : null,
                ],
                ['Client IP', event.client_ip],
                ['User-Agent', event.user_agent],
                ['Referrer', event.referrer],
              ]}
            />
          </Section>
          <HeadersBlock
            label="Request headers"
            headers={event.request_headers}
            truncated={event.headers_truncated}
          />
          <HeadersBlock
            label="Response headers"
            headers={event.response_headers}
            truncated={event.headers_truncated}
          />
          {event.error_group_id != null && (
            <Section title="Linked error group">
              <Link
                to={`/projects/${projectSlug}/errors/${event.error_group_id}`}
                className="inline-flex items-center gap-1 text-sm text-primary hover:underline"
              >
                Open error group #{event.error_group_id}
                <ExternalLink className="h-3 w-3" />
              </Link>
            </Section>
          )}
        </>
      )
    case 'span':
      return (
        <>
          <Section title="Span">
            <KvList
              items={[
                ['Service', event.service],
                ['Operation', event.operation],
                ['Span ID', event.span_id],
                ['Parent span', event.parent_span_id],
                ['Status', event.status],
                [
                  'Duration',
                  event.duration_ms != null
                    ? `${event.duration_ms.toFixed(1)}ms`
                    : null,
                ],
              ]}
            />
          </Section>
          <Section title="Attributes">
            <pre className="overflow-x-auto rounded bg-muted p-3 text-xs">
              {JSON.stringify(event.attributes, null, 2)}
            </pre>
            {event.attributes_truncated && (
              <p className="text-xs text-muted-foreground">
                Showing first attributes only — open the trace for the full
                set.
              </p>
            )}
          </Section>
        </>
      )
    case 'error':
      return (
        <>
          <Section title="Exception">
            <KvList
              items={[
                ['Class', event.error_class],
                ['Message', event.message],
                ['Fingerprint', event.fingerprint],
              ]}
            />
            <Link
              to={`/projects/${projectSlug}/errors/${event.error_group_id}`}
              className="mt-2 inline-flex items-center gap-1 text-sm text-primary hover:underline"
            >
              Open error group #{event.error_group_id}
              <ExternalLink className="h-3 w-3" />
            </Link>
          </Section>
          <Section title="Stack trace (preview)">
            <pre className="overflow-x-auto rounded bg-muted p-3 text-xs">
              {JSON.stringify(event.stacktrace_preview, null, 2)}
            </pre>
            {event.stacktrace_truncated && (
              <Button asChild variant="link" size="sm" className="px-0">
                <Link
                  to={`/projects/${projectSlug}/errors/${event.error_group_id}`}
                >
                  Show full stack trace
                </Link>
              </Button>
            )}
          </Section>
        </>
      )
    case 'revenue':
      return (
        <Section title="Revenue event">
          <KvList
            items={[
              ['Provider', event.provider],
              ['Event type', event.event_type],
              ['Customer', event.customer_ref],
              [
                'Amount',
                event.amount_minor != null
                  ? `${(event.amount_minor / 100).toFixed(2)} ${
                      event.currency?.toUpperCase() ?? ''
                    }`
                  : null,
              ],
            ]}
          />
        </Section>
      )
  }
}

function HeadersBlock({
  label,
  headers,
  truncated,
}: {
  label: string
  // The wire type is `unknown` (serde_json::Value); we narrow to a record at runtime.
  headers: unknown
  truncated: boolean
}) {
  const entries =
    headers && typeof headers === 'object'
      ? Object.entries(headers as Record<string, unknown>)
      : []
  if (entries.length === 0) return null
  return (
    <Section title={label}>
      <pre className="overflow-x-auto rounded bg-muted p-3 text-xs">
        {entries.map(([k, v]) => `${k}: ${String(v)}`).join('\n')}
      </pre>
      {truncated && (
        <p className="text-xs text-muted-foreground">
          Whitelisted headers only.{' '}
          <Badge variant="outline" className="ml-1">
            Truncated
          </Badge>
        </p>
      )}
    </Section>
  )
}

function Section({
  title,
  children,
}: {
  title: string
  children: React.ReactNode
}) {
  return (
    <section className="space-y-2">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      {children}
    </section>
  )
}

function KvList({ items }: { items: Array<[string, string | null | undefined]> }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      {items
        .filter(([, v]) => v != null && v !== '')
        .map(([k, v]) => (
          <Fragment key={k}>
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="break-all font-mono text-xs">{v}</dd>
          </Fragment>
        ))}
    </dl>
  )
}
