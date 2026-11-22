// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { SetupGuide, SnippetTabs } from '@/components/onboarding/SetupGuide'
import { CodeBlock } from '@/components/ui/code-block'
import {
  defineSnippet,
  devCommand,
  scaffoldCommand,
  setupStacks,
  startSnippet,
  type SetupTarget,
} from './setup-snippets'

export interface WorkflowsSetupGuideProps {
  title: string
  /** The environment the steps refer to; without one the guide is generic. */
  target?: SetupTarget
  credentialConfigured?: boolean
  executorReady?: boolean
  /** Why the executor is not reachable, as the server reports it. */
  executorReason?: string | null
  /** Creates the credential (or links to where it is created). */
  credentialAction: ReactNode
}

/** How to connect an app to durable workflows, step by step. */
export function WorkflowsSetupGuide({
  title,
  target,
  credentialConfigured = false,
  executorReady = false,
  executorReason,
  credentialAction,
}: WorkflowsSetupGuideProps) {
  return (
    <SetupGuide
      title={title}
      steps={[
        {
          title: 'Create a workflow credential',
          done: credentialConfigured,
          caption: credentialConfigured
            ? 'Set it as the secret environment variable TEMPS_WORKFLOWS_TOKEN.'
            : 'Your app uses it to talk to Temps; store it as the secret TEMPS_WORKFLOWS_TOKEN.',
          content: credentialAction,
        },
        {
          title: 'Define a workflow and serve it',
          caption:
            'Finished steps are recorded and replayed, but a step can run again before it finishes, so make it safe to repeat. Sleeps and waits hold nothing in memory.',
          content: (
            <>
              <SnippetTabs
                snippets={setupStacks.map((stack) => ({
                  ...stack,
                  ...defineSnippet(stack.value),
                }))}
              />
              <p className="text-sm text-muted-foreground">
                Starting from scratch? Scaffold a Node worker with a test and a
                Dockerfile:
              </p>
              <CodeBlock code={scaffoldCommand} language="bash" />
            </>
          ),
        },
        {
          title: 'Deploy it, or run it on your machine',
          done: executorReady,
          caption: executorReady
            ? 'Temps can reach your workflows.'
            : (executorReason ??
              'Deploy the app to this environment, or forward a preview environment to a local server:'),
          content: (
            <CodeBlock code={devCommand(target ?? {})} language="bash" />
          ),
        },
        {
          title: 'Start a run',
          caption:
            'From your app; once deployed, the workflow is listed here and can be started from this page too.',
          content: <CodeBlock code={startSnippet} language="typescript" />,
        },
      ]}
    />
  )
}
