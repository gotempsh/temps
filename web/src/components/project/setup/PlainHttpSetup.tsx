// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The "Plain HTTP / any framework" setup of one telemetry signal: endpoints,
 * auth, payloads, a working curl and a "Copy prompt" for AI coding agents,
 * all from the server-resolved `GET /projects/{id}/ingest-setup`.
 */
import { Link } from 'react-router'
import { Globe, KeyRound } from 'lucide-react'
import type { IngestSignalSetup } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import { CopyAiPromptButton } from '@/components/ui/ai-prompt-code-block'
import { Skeleton } from '@/components/ui/skeleton'
import { monitoringSetupPrompt } from '@/lib/monitoring-setup-prompt'
import {
  analyticsBrowserSnippet,
  analyticsPayloads,
  otelCollectorConfig,
  otlpEnvVars,
  verificationCurl,
  type IngestSignalName,
} from '@/lib/plain-http-setup'

import { findSignal, useIngestSetup } from './use-ingest-setup'

type ProjectRef = { id: number; name: string; slug: string }

const titles: Record<IngestSignalName, string> = {
  analytics: 'Analytics',
  session_replay: 'Session replay',
  errors: 'Error tracking',
  logs: 'Logs',
  traces: 'Traces',
}

/** "Copy prompt" for a signal, disabled until the setup has loaded. */
export function CopySetupPromptButton({
  project,
  signal,
  environmentId,
}: {
  project: ProjectRef
  signal: IngestSignalName
  environmentId?: number
}) {
  const query = useIngestSetup(project.id, environmentId)
  const setup = findSignal(query.data, signal)
  if (!setup) {
    return (
      <Button variant="outline" size="sm" disabled>
        {query.isError ? 'Prompt unavailable' : 'Copy AI prompt'}
      </Button>
    )
  }
  return (
    <CopyAiPromptButton
      prompt={monitoringSetupPrompt(
        signal,
        project,
        window.location.origin,
        setup
      )}
    />
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
    <div className="space-y-2">
      <h4 className="text-sm font-medium">{title}</h4>
      {children}
    </div>
  )
}

/** Presentational view, so it renders without a query client in tests. */
export function PlainHttpSetupView({
  project,
  setup,
  origin,
  nowMs,
}: {
  project: ProjectRef
  setup: IngestSignalSetup
  origin: string
  nowMs?: number
}) {
  const prompt = monitoringSetupPrompt(setup.signal, project, origin, setup)
  const isOtlp = setup.signal === 'logs' || setup.signal === 'traces'
  return (
    <div className="space-y-6" data-testid={`plain-http-${setup.signal}`}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Globe className="size-4 shrink-0 text-muted-foreground" />
          <p className="text-sm font-medium">
            {titles[setup.signal]} over plain HTTP
          </p>
        </div>
        <CopyAiPromptButton prompt={prompt} />
      </div>

      {!setup.configured && (
        <Alert>
          <KeyRound className="size-4" />
          <AlertTitle>{setup.reason}</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-2">
            <span>
              Create one, then read it from{' '}
              <code className="text-xs">${setup.auth.env_var}</code>.
            </span>
            <Button asChild size="sm" variant="outline">
              <Link to={setup.auth.credential_path}>Create credential</Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      <Section title="Endpoints">
        <ul className="space-y-1 text-sm">
          {setup.endpoints.map((e) => (
            <li key={e.name} className="[overflow-wrap:anywhere]">
              <code className="mr-2 rounded bg-muted px-1.5 py-0.5 text-xs font-semibold">
                {e.method}
              </code>
              <code className="text-xs">{e.url}</code>
              <span className="ml-2 text-xs text-muted-foreground">
                {e.content_type}
              </span>
            </li>
          ))}
        </ul>
      </Section>

      <Section title="Auth">
        <CodeBlock
          language="text"
          code={setup.auth.headers
            .map((h) => `${h.name}: ${h.value}`)
            .join('\n')}
        />
        <p className="text-xs text-muted-foreground">
          {setup.auth.browser_safe
            ? `Public, write-only key: safe in browser code.`
            : `Secret: server-side or Collector only, never in browser code.`}{' '}
          <Link
            className="underline underline-offset-4"
            to={setup.auth.credential_path}
          >
            {setup.auth.browser_safe ? 'Get the key' : 'Create an API key'}
          </Link>{' '}
          or run:
        </p>
        <CodeBlock language="bash" code={setup.auth.cli_command} />
      </Section>

      {setup.signal === 'analytics' && (
        <>
          <Section title="Payload">
            <CodeBlock
              language="json"
              code={JSON.stringify(analyticsPayloads(), null, 2)}
            />
          </Section>
          <Section title="Browser (fetch + sendBeacon)">
            <CodeBlock
              language="javascript"
              code={analyticsBrowserSnippet(setup)}
            />
          </Section>
        </>
      )}

      {setup.signal === 'errors' && (
        <p className="text-sm text-muted-foreground">
          Any Sentry SDK works: set its DSN from{' '}
          <code className="text-xs">${setup.auth.env_var}</code>.
        </p>
      )}

      {isOtlp && (
        <>
          <p className="text-sm text-muted-foreground">
            OTLP/HTTP protobuf. Any OpenTelemetry SDK or the OTel Collector
            works.
          </p>
          <Section title="OpenTelemetry SDK">
            <CodeBlock
              language="bash"
              code={otlpEnvVars(setup, project.slug)}
              title=".env"
            />
          </Section>
          <Section title="OTel Collector">
            <CodeBlock language="yaml" code={otelCollectorConfig(setup)} />
          </Section>
        </>
      )}

      <Section title="Test with curl">
        <CodeBlock
          language="bash"
          code={verificationCurl(setup, project.slug, nowMs)}
        />
      </Section>
    </div>
  )
}

/** The plain HTTP option of a setup flow, loading its own setup. */
export function PlainHttpSetup({
  project,
  signal,
  environmentId,
}: {
  project: ProjectRef
  signal: IngestSignalName
  environmentId?: number
}) {
  const query = useIngestSetup(project.id, environmentId)
  const setup = findSignal(query.data, signal)

  if (query.isPending) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-5 w-48" />
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-24 w-full" />
      </div>
    )
  }
  if (query.isError || !setup) {
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not load the ingest endpoints</AlertTitle>
        <AlertDescription className="flex flex-wrap items-center gap-2">
          <span>{query.error?.detail ?? 'Try again.'}</span>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void query.refetch()}
          >
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  }
  return (
    <PlainHttpSetupView
      project={project}
      setup={setup}
      origin={window.location.origin}
    />
  )
}
