// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getAiHarnessStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { CopyButton } from '@/components/ui/copy-button'
import { describeHarnessConnection } from '@/lib/ai-onboarding'
import { problemDetail } from '@/lib/api-problem'
import type { McpAppId } from '@/lib/mcp-clients'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { Button, Callout } from '@temps-sdk/ds'
import { AppWindow, SquareTerminal } from 'lucide-react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { type ReactNode, useState } from 'react'
import {
  AGENT_PATHS,
  type AgentPath,
  AGENT_PATH_LOGOS,
  agentConnection,
  mcpClientFor,
} from './agents'
import { LogoStack } from './LogoStack'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ClaudeAppSetup } from './ClaudeAppSetup'
import { OtherAppSetup } from './OtherAppSetup'
import {
  LATER_REASONS,
  connectAgentPrompt,
  harnessCommands,
  reachHint,
} from './onboarding'
import { useUpdateOnboarding } from './useOnboarding'

/**
 * One mark per tab, beside its label (above it on a phone): a terminal for coding agents (there are
 * many), Claude's logo for the Claude app, a window for other apps. The tab's
 * panel shows the logos of the apps it covers.
 */
const TAB_ICONS: Record<AgentPath, ReactNode> = {
  coding_agent: <SquareTerminal className="size-4" />,
  claude_app: <AiHarnessLogo providerId="claude_cli" size={16} />,
  other_app: <AppWindow className="size-4" />,
}

/**
 * Step 3: connect the agent you already use. A coding agent gets a prompt
 * (or two commands) that log it in to this console; the Claude app gets the
 * Temps extension and a read-only key. Continue works once either has
 * connected. "Do this later" leaves the step for the Setup page.
 */
export function AgentsStep({ onContinue }: { onContinue: () => void }) {
  const [path, setPath] = useState<AgentPath>('coding_agent')
  // Held here so the connection is checked against the key of the app
  // picked under Another app, not any app's.
  const [otherApp, setOtherApp] = useState<McpAppId>('cursor')
  const client = mcpClientFor(path, otherApp)
  const harness = useQuery({
    ...getAiHarnessStatusOptions(),
    // Polls until the picked path is connected: on the Claude app path an
    // earlier CLI login is not enough, so it keeps watching for its key.
    refetchInterval: (query) =>
      agentConnection(path, query.state.data, client).kind === 'connected'
        ? false
        : 3000,
  })
  const updateOnboarding = useUpdateOnboarding()

  const done = useMutation({
    mutationFn: () => updateOnboarding.mutateAsync({ done: ['agents'] }),
    onSuccess: onContinue,
  })
  const skip = useMutation({
    mutationFn: () =>
      updateOnboarding.mutateAsync({
        later: [{ step: 'agents', reason: LATER_REASONS.agents }],
      }),
    onSuccess: onContinue,
  })

  const commands = harnessCommands(window.location.origin)
  const prompt = connectAgentPrompt(window.location.origin)
  const hint = reachHint(window.location.origin)
  const connection = agentConnection(path, harness.data, client)
  const connected = connection.kind === 'connected'
  const canContinue =
    connected || (connection.kind === 'waiting' && connection.canContinue)
  const pending = done.isPending || skip.isPending

  return (
    <div className="space-y-6">
      {/* The stepper already names the step; the heading asks its question. */}
      <h2 id="agents-question" className="text-lg font-semibold">
        What do you use?
      </h2>

      <div className="space-y-4">
        <Tabs
          value={path}
          onValueChange={(value) => setPath(value as AgentPath)}
        >
          <TabsList
            aria-labelledby="agents-question"
            className="grid grid-cols-3 gap-0 sm:flex sm:gap-5"
          >
            {AGENT_PATHS.map((choice) => (
              <TabsTrigger
                key={choice.id}
                value={choice.id}
                // On a phone the mark sits above the label, so three tabs
                // fit side by side without scrolling.
                className="w-full flex-col gap-1 py-2 sm:w-auto sm:flex-row sm:gap-1.5 sm:py-3"
              >
                <span className="shrink-0" aria-hidden>
                  {TAB_ICONS[choice.id]}
                </span>
                {choice.label}
              </TabsTrigger>
            ))}
          </TabsList>
          {AGENT_PATHS.map((choice) => (
            <TabsContent
              key={choice.id}
              value={choice.id}
              className="space-y-4"
            >
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                <LogoStack ids={AGENT_PATH_LOGOS[choice.id]} />
                <p className="text-sm text-muted-foreground">{choice.detail}</p>
              </div>
              {choice.id === 'claude_app' ? (
                <ClaudeAppSetup
                  onUseCodingAgent={() => setPath('coding_agent')}
                />
              ) : choice.id === 'other_app' ? (
                <OtherAppSetup appId={otherApp} onAppChange={setOtherApp} />
              ) : (
                <CodingAgentSetup
                  prompt={prompt}
                  hint={hint}
                  commands={commands}
                />
              )}
            </TabsContent>
          ))}
        </Tabs>
        {connected ? (
          <Callout tone="success" title="Your agent is connected">
            {describeHarnessConnection(harness.data?.connected_via)}
          </Callout>
        ) : harness.isError ? (
          <p className="text-sm text-destructive" role="status">
            {problemDetail(
              harness.error,
              'Could not check whether your agent has logged in.'
            )}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground" role="status">
            {connection.kind === 'waiting' ? connection.line : null}
          </p>
        )}
      </div>

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <Button
          busy={done.isPending}
          busyLabel="Saving…"
          aria-disabled={!canContinue || pending || undefined}
          onClick={() => canContinue && !pending && done.mutate()}
        >
          Continue
        </Button>
        <Button
          variant="ghost"
          busy={skip.isPending}
          busyLabel="Saving…"
          aria-disabled={pending || undefined}
          onClick={() => !pending && skip.mutate()}
        >
          Do this later
        </Button>
        {(skip.isError || done.isError) && (
          <p className="text-sm text-destructive">
            {problemDetail(
              skip.error ?? done.error,
              'Could not save. Try again.'
            )}
          </p>
        )}
      </div>
    </div>
  )
}

/** The coding-agent path: one prompt to paste, or the commands it runs. */
function CodingAgentSetup({
  prompt,
  hint,
  commands,
}: {
  prompt: string
  hint: string | null
  commands: string[]
}) {
  return (
    <>
      <div className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p className="text-sm font-medium">
            Paste this prompt into your agent
          </p>
          <CopyButton
            value={prompt}
            minimal
            label="Copy prompt"
            className="h-8 rounded-md border px-3"
          >
            Copy prompt
          </CopyButton>
        </div>
        {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
        {/* Focusable, so the keyboard can scroll it. */}
        <pre
          role="region"
          aria-label="Prompt for your agent"
          tabIndex={0}
          className="max-h-48 overflow-y-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs [overflow-wrap:anywhere]"
        >
          {prompt}
        </pre>
      </div>
      <p className="text-sm text-muted-foreground">
        Or run these yourself. The login creates an API key for your agent.
      </p>
      <ol className="space-y-2">
        {commands.map((command) => (
          <li
            key={command}
            className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2"
          >
            <code className="min-w-0 flex-1 font-mono text-xs [overflow-wrap:anywhere]">
              {command}
            </code>
            <CopyButton value={command} />
          </li>
        ))}
      </ol>
    </>
  )
}
