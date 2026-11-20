// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@temps-sdk/ds'
import { Check } from 'lucide-react'
import { CodeBlock } from '@/components/ui/code-block'
import { cn } from '@/lib/utils'
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

/**
 * How to connect an app to durable workflows, as numbered steps, each with
 * the code or command that does it. It is the empty state of a workflows
 * list, so it sits inside that list's one surface rather than in cards.
 */
export function WorkflowsSetupGuide({
  title,
  target,
  credentialConfigured = false,
  executorReady = false,
  executorReason,
  credentialAction,
}: WorkflowsSetupGuideProps) {
  return (
    <section
      aria-labelledby="workflows-setup-title"
      className="rounded-lg border bg-card text-card-foreground"
    >
      <h2
        id="workflows-setup-title"
        className="border-b px-4 py-3 text-base font-medium"
      >
        {title}
      </h2>
      <ol className="divide-y">
        <SetupStep
          number={1}
          done={credentialConfigured}
          title="Create a workflow credential"
          caption={
            credentialConfigured
              ? 'Set it as the secret environment variable TEMPS_WORKFLOWS_TOKEN.'
              : 'Your app uses it to talk to Temps; store it as the secret TEMPS_WORKFLOWS_TOKEN.'
          }
        >
          {credentialAction}
        </SetupStep>
        <SetupStep
          number={2}
          title="Define a workflow and serve it"
          caption="Each step runs once and is recorded; sleeps and waits hold nothing in memory."
        >
          <Tabs defaultValue="node" className="w-full">
            <TabsList>
              {setupStacks.map((stack) => (
                <TabsTrigger key={stack.value} value={stack.value}>
                  {stack.label}
                </TabsTrigger>
              ))}
            </TabsList>
            {setupStacks.map((stack) => {
              const snippet = defineSnippet(stack.value)
              return (
                <TabsContent
                  key={stack.value}
                  value={stack.value}
                  className="mt-3"
                >
                  <CodeBlock code={snippet.code} language={snippet.language} />
                </TabsContent>
              )
            })}
          </Tabs>
          <p className="text-sm text-muted-foreground">
            Starting from scratch? Scaffold a Node worker with a test and a
            Dockerfile:
          </p>
          <CodeBlock code={scaffoldCommand} language="bash" />
        </SetupStep>
        <SetupStep
          number={3}
          done={executorReady}
          title="Deploy it, or run it on your machine"
          caption={
            executorReady
              ? 'Temps can reach your workflows.'
              : (executorReason ??
                'Deploy the app to this environment, or forward a preview environment to a local server:')
          }
        >
          <CodeBlock code={devCommand(target ?? {})} language="bash" />
        </SetupStep>
        <SetupStep
          number={4}
          title="Start a run"
          caption="From your app; once deployed, the workflow is listed here and can be started from this page too."
        >
          <CodeBlock code={startSnippet} language="typescript" />
        </SetupStep>
      </ol>
    </section>
  )
}

function SetupStep({
  number,
  title,
  caption,
  done = false,
  children,
}: {
  number: number
  title: string
  caption: ReactNode
  done?: boolean
  children: ReactNode
}) {
  return (
    <li className="flex gap-3 px-4 py-4">
      <span
        aria-hidden
        className={cn(
          'flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-medium',
          done && 'border-transparent bg-primary text-primary-foreground'
        )}
      >
        {done ? <Check className="size-3.5" /> : number}
      </span>
      <div className="min-w-0 flex-1 space-y-3">
        <div className="space-y-0.5">
          <h3 className="text-sm font-medium">
            {title}
            {done && <span className="sr-only"> (done)</span>}
          </h3>
          <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">
            {caption}
          </p>
        </div>
        {children}
      </div>
    </li>
  )
}
