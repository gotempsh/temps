// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { getProjectOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  SetupGuide,
  SnippetTabs,
  type SetupGuideStep,
} from '@/components/onboarding/SetupGuide'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import {
  analyticsSnippets,
  errorSnippets,
  logSnippet,
  serviceSetupHref,
  testErrorSnippet,
  traceEnvironment,
  traceSnippets,
  type ObservabilityKind,
} from './observability-setup'

/**
 * The empty state of a global analytics, errors, traces or logs page: how to
 * start sending that data, step by step. With a service in scope, its links
 * open that service's own setup page, where keys and endpoints are filled in.
 */
export function ObservabilitySetupGuide({
  kind,
  projectId,
}: {
  kind: ObservabilityKind
  projectId?: number
}) {
  const project = useQuery({
    ...getProjectOptions({ path: { id: projectId ?? 0 } }),
    enabled: projectId !== undefined,
  })
  const service = project.data
  const setupLink = (label: string) => (
    <Button asChild variant="outline">
      <Link to={serviceSetupHref(kind, service?.slug)}>
        {service ? `${label} for ${service.name}` : 'Choose a service'}
      </Link>
    </Button>
  )
  const guide = guides[kind](setupLink, service?.id)
  return (
    <SetupGuide
      title={guide.title}
      description={guide.description}
      steps={guide.steps}
    />
  )
}

type Guide = { title: string; description: string; steps: SetupGuideStep[] }

const guides: Record<
  ObservabilityKind,
  (setupLink: (label: string) => ReactNode, projectId?: number) => Guide
> = {
  analytics: (setupLink) => ({
    title: 'No analytics yet',
    description: 'No visit arrived in the selected time range.',
    steps: [
      {
        title: 'Open the service’s analytics setup',
        caption:
          'It has the base path and ingest key for the service, already filled into the snippets.',
        content: setupLink('Open analytics setup'),
      },
      {
        title: 'Add the analytics script to your site',
        caption: 'It records page views, sessions and visitors.',
        content: <SnippetTabs snippets={analyticsSnippets} />,
      },
      {
        title: 'Visit a page',
        caption:
          'Open your site in a browser, then refresh this page to see the visit.',
      },
    ],
  }),
  errors: (setupLink) => ({
    title: 'No errors reported',
    description:
      'No error arrived in the selected time range. If your app does not report errors yet, connect it:',
    steps: [
      {
        title: 'Install a Sentry SDK',
        caption:
          'Temps accepts the Sentry protocol, so the official Sentry SDKs work unchanged.',
        content: <SnippetTabs snippets={errorSnippets} />,
      },
      {
        title: 'Set the DSN',
        caption:
          'Apps deployed on Temps get SENTRY_DSN automatically. Anywhere else, copy it from the service’s error tracking setup.',
        content: setupLink('Open error tracking setup'),
      },
      {
        title: 'Send a test error',
        caption: 'It shows here, grouped with any repeats.',
        content: <CodeBlock code={testErrorSnippet} language="typescript" />,
      },
    ],
  }),
  traces: (setupLink, projectId) => ({
    title: 'No traces yet',
    description: 'No trace arrived in the selected time range.',
    steps: [
      {
        title: 'Add OpenTelemetry to your app',
        caption: 'The standard SDK for your language; nothing Temps-specific.',
        content: <SnippetTabs snippets={traceSnippets} />,
      },
      {
        title: 'Point it at Temps',
        caption:
          'Apps deployed on Temps get these variables automatically. Anywhere else, set them with an API key:',
        content: (
          <>
            <CodeBlock code={traceEnvironment(projectId)} language="bash" />
            {setupLink('Open trace setup')}
          </>
        ),
      },
      {
        title: 'Make a request',
        caption:
          'Each request your app serves becomes a trace, followed across services.',
      },
    ],
  }),
  logs: (setupLink) => ({
    title: 'No logs yet',
    description: 'Nothing was logged in the selected time range.',
    steps: [
      {
        title: 'Run a service or database on Temps',
        caption:
          'Temps collects the output of every container it runs; there is nothing to install.',
        content: (
          <div className="flex flex-wrap gap-2">
            <Button asChild variant="outline">
              <Link to="/projects/new">New project</Link>
            </Button>
            {setupLink('Open deployments')}
          </div>
        ),
      },
      {
        title: 'Write to standard output',
        caption:
          'Print to stdout or stderr; each line appears here with its service and environment.',
        content: <CodeBlock code={logSnippet} language="typescript" />,
      },
    ],
  }),
}
