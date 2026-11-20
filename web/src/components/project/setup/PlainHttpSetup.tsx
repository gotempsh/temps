// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The "any framework" setup of one telemetry signal, library first: the
 * credential, then the SDK, OpenTelemetry or browser code that sends data,
 * with endpoints and a curl test folded away for clients without a library.
 * Everything comes from the server-resolved `GET /projects/{id}/ingest-setup`.
 */
import { Link } from 'react-router'
import {
  ChevronRight,
  Code2,
  FileCode,
  KeyRound,
  Package,
  RotateCw,
  type LucideIcon,
} from 'lucide-react'
import type { IngestSignalSetup } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CodeBlock, InlineCode } from '@/components/ui/code-block'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { CopyAiPromptButton } from '@/components/ui/ai-prompt-code-block'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ScrollableTabsList,
  Tabs,
  TabsContent,
  TabsTrigger,
} from '@/components/ui/tabs'
import { monitoringSetupPrompt } from '@/lib/monitoring-setup-prompt'
import {
  analyticsBrowserSnippet,
  analyticsPayloads,
  otelCollectorConfig,
  otlpEnvVars,
  sentrySdkExamples,
  sessionReplayExamples,
  verificationCurl,
  type IngestSignalName,
  type LibraryExample,
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
  if (!setup && query.isError) {
    // The prompt needs the server-resolved endpoints; say so and let the
    // user ask for them again.
    return (
      <Button
        variant="outline"
        size="sm"
        title="Could not load the ingest endpoints for the prompt"
        disabled={query.isFetching}
        onClick={() => void query.refetch()}
      >
        <RotateCw className="size-3.5" />
        Retry prompt
      </Button>
    )
  }
  if (!setup) {
    return (
      <Button variant="outline" size="sm" disabled>
        Copy AI prompt
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

/** A numbered step, styled like the SDK steps of the setup wizards. */
function Step({
  icon: Icon,
  title,
  children,
}: {
  icon: LucideIcon
  title: string
  children: React.ReactNode
}) {
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <Icon className="size-4 shrink-0 text-muted-foreground" />
        <h3 className="text-sm font-medium">{title}</h3>
      </div>
      {children}
    </div>
  )
}

/** Library examples in tabs, one code block per language or tool. */
function LibraryTabs({
  label,
  examples,
}: {
  label: string
  examples: LibraryExample[]
}) {
  return (
    <Tabs defaultValue={examples[0]?.id}>
      <ScrollableTabsList aria-label={label}>
        {examples.map((example) => (
          <TabsTrigger key={example.id} value={example.id}>
            {example.label}
          </TabsTrigger>
        ))}
      </ScrollableTabsList>
      {examples.map((example) => (
        <TabsContent key={example.id} value={example.id} className="pt-3">
          <CodeBlock
            defaultWrap
            title={example.file}
            language={example.language}
            code={example.code}
          />
        </TabsContent>
      ))}
    </Tabs>
  )
}

const libraryTitles: Record<IngestSignalName, string> = {
  analytics: 'Track from any browser code',
  session_replay: 'Record with a library',
  errors: 'Install a Sentry SDK',
  logs: 'Export with OpenTelemetry',
  traces: 'Export with OpenTelemetry',
}

/** Step 2: the library (or, for analytics, browser code) that sends data. */
function LibraryStep({
  setup,
  serviceName,
}: {
  setup: IngestSignalSetup
  serviceName: string
}) {
  const env = <InlineCode className="text-xs">${setup.auth.env_var}</InlineCode>
  switch (setup.signal) {
    case 'errors':
      return (
        <>
          <p className="text-sm text-muted-foreground">
            Temps speaks the Sentry protocol: any official Sentry SDK works. Set
            its DSN from {env} and change nothing else.{' '}
            <a
              className="underline underline-offset-4"
              href="https://docs.sentry.io/platforms/"
              target="_blank"
              rel="noreferrer"
            >
              All Sentry SDKs
            </a>
          </p>
          <LibraryTabs
            label="Sentry SDK examples"
            examples={sentrySdkExamples(setup)}
          />
        </>
      )
    case 'logs':
    case 'traces':
      return (
        <>
          <p className="text-sm text-muted-foreground">
            Any OpenTelemetry SDK reads these variables, or point an OTel
            Collector at Temps. OTLP/HTTP with protobuf.
          </p>
          <LibraryTabs
            label="OpenTelemetry examples"
            examples={[
              {
                id: 'sdk',
                label: 'OpenTelemetry SDK',
                file: '.env',
                language: 'bash',
                code: otlpEnvVars(setup, serviceName),
              },
              {
                id: 'collector',
                label: 'OTel Collector',
                file: 'otel-collector.yaml',
                language: 'yaml',
                code: otelCollectorConfig(setup),
              },
            ]}
          />
        </>
      )
    case 'session_replay':
      return (
        <>
          <p className="text-sm text-muted-foreground">
            Record only after consent where the law requires it, mask inputs,
            and add the <InlineCode className="text-xs">rr-block</InlineCode>{' '}
            class to anything showing personal or payment data.
          </p>
          <LibraryTabs
            label="Session replay examples"
            examples={sessionReplayExamples(setup)}
          />
        </>
      )
    case 'analytics':
      return (
        <>
          <p className="text-sm text-muted-foreground">
            Without the SDK: <InlineCode className="text-xs">fetch</InlineCode>{' '}
            for page views and events,{' '}
            <InlineCode className="text-xs">sendBeacon</InlineCode> when the
            page closes.
          </p>
          <CodeBlock
            defaultWrap
            title="analytics.js"
            language="javascript"
            code={analyticsBrowserSnippet(setup)}
          />
        </>
      )
  }
}

/** Endpoints, headers, payload and a curl test, for clients without a library. */
function RawHttpDetails({
  setup,
  serviceName,
  nowMs,
}: {
  setup: IngestSignalSetup
  serviceName: string
  nowMs?: number
}) {
  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-1 gap-x-4 gap-y-2 text-sm sm:grid-cols-[auto_1fr]">
        <dt className="text-muted-foreground">Endpoints</dt>
        <dd className="min-w-0 space-y-1">
          {setup.endpoints.map((e) => (
            <div key={e.name} className="[overflow-wrap:anywhere]">
              <Badge variant="secondary" className="mr-2 font-mono text-[11px]">
                {e.method}
              </Badge>
              <InlineCode className="text-xs">{e.url}</InlineCode>{' '}
              <span className="text-xs text-muted-foreground">
                {e.content_type}
              </span>
            </div>
          ))}
        </dd>
        <dt className="text-muted-foreground">Headers</dt>
        <dd className="min-w-0 space-y-1 [overflow-wrap:anywhere]">
          {setup.auth.headers.map((h) => (
            <div key={h.name}>
              <InlineCode className="text-xs">
                {h.name}: {h.value}
              </InlineCode>
            </div>
          ))}
        </dd>
      </dl>
      {setup.signal === 'analytics' && (
        <CodeBlock
          defaultWrap
          title="payload.json"
          language="json"
          code={JSON.stringify(analyticsPayloads(), null, 2)}
        />
      )}
      <CodeBlock
        defaultWrap
        title="Test with curl"
        language="bash"
        code={verificationCurl(setup, serviceName, nowMs)}
      />
    </div>
  )
}

/** Presentational view, so it renders without a query client in tests. */
export function PlainHttpSetupView({
  project,
  setup,
  origin,
  nowMs,
  defaultRawOpen = false,
  credentialValue,
  showCredentialStep = true,
}: {
  project: ProjectRef
  setup: IngestSignalSetup
  origin: string
  nowMs?: number
  /** Start with the raw HTTP details expanded. */
  defaultRawOpen?: boolean
  /**
   * The credential when the page already holds it and it is public (an
   * error DSN): step 1 then shows it as a `.env` line.
   */
  credentialValue?: string
  /** False when the page already shows the credential above this view. */
  showCredentialStep?: boolean
}) {
  const prompt = monitoringSetupPrompt(setup.signal, project, origin, setup)
  return (
    <div className="space-y-6" data-testid={`plain-http-${setup.signal}`}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Code2 className="size-4 shrink-0 text-muted-foreground" />
          <p className="text-sm font-medium">
            {titles[setup.signal]} from any language
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
              <InlineCode className="text-xs">${setup.auth.env_var}</InlineCode>
              .
            </span>
            <Button asChild size="sm" variant="outline">
              <Link to={setup.auth.credential_path}>Create credential</Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {showCredentialStep && (
        <Step icon={KeyRound} title="1. Get the credential">
          <p className="text-sm text-muted-foreground">
            {setup.auth.browser_safe
              ? 'Public, write-only key: safe in browser code.'
              : 'Secret: use it server-side or in the Collector, never in browser code.'}{' '}
            Read it from{' '}
            <InlineCode className="text-xs">${setup.auth.env_var}</InlineCode>.{' '}
            <Link
              className="underline underline-offset-4"
              to={setup.auth.credential_path}
            >
              {setup.auth.browser_safe ? 'Get the key' : 'Create an API key'}
            </Link>{' '}
            {credentialValue ? 'or use this one:' : 'or run:'}
          </p>
          {credentialValue ? (
            <CodeBlock
              defaultWrap
              title=".env"
              language="bash"
              code={`${setup.auth.env_var}=${credentialValue}`}
            />
          ) : (
            <CodeBlock
              defaultWrap
              title="terminal"
              language="bash"
              code={setup.auth.cli_command}
            />
          )}
        </Step>
      )}

      <Step
        icon={setup.signal === 'analytics' ? FileCode : Package}
        title={
          showCredentialStep
            ? `2. ${libraryTitles[setup.signal]}`
            : libraryTitles[setup.signal]
        }
      >
        <LibraryStep setup={setup} serviceName={project.slug} />
      </Step>

      <Collapsible defaultOpen={defaultRawOpen} className="group/raw">
        <CollapsibleTrigger asChild>
          <Button variant="ghost" size="sm" className="-ml-2">
            <ChevronRight className="size-4 transition-transform group-data-[state=open]/raw:rotate-90" />
            Without a library: endpoints and a curl test
          </Button>
        </CollapsibleTrigger>
        <CollapsibleContent className="pt-3">
          <RawHttpDetails
            setup={setup}
            serviceName={project.slug}
            nowMs={nowMs}
          />
        </CollapsibleContent>
      </Collapsible>
    </div>
  )
}

/** The plain HTTP option of a setup flow, loading its own setup. */
export function PlainHttpSetup({
  project,
  signal,
  environmentId,
  credentialValue,
  showCredentialStep,
}: {
  project: ProjectRef
  signal: IngestSignalName
  environmentId?: number
  credentialValue?: string
  showCredentialStep?: boolean
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
  if (!setup) {
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
  // A failed background refresh keeps the instructions that already loaded.
  return (
    <div className="space-y-4">
      {query.isError && (
        <Alert>
          <AlertTitle>Could not refresh the ingest endpoints</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-2">
            <span>Showing the last loaded setup.</span>
            <Button
              size="sm"
              variant="outline"
              disabled={query.isFetching}
              onClick={() => void query.refetch()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}
      <PlainHttpSetupView
        project={project}
        setup={setup}
        origin={window.location.origin}
        credentialValue={credentialValue}
        showCredentialStep={showCredentialStep}
      />
    </div>
  )
}
