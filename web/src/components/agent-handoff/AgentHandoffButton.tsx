// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getSettingsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { useAuth } from '@/contexts/AuthContext'
import {
  HAND_OFF_LABEL,
  MAX_REQUEST_LENGTH,
  buildAgentHandoffPrompt,
} from '@/lib/agent-handoff'
import { TEMPS_CLI_VERSION } from '@/lib/ai-onboarding'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { ShieldAlert, SquareTerminal } from 'lucide-react'
import { useId, useState } from 'react'
import { Link, useLocation } from 'react-router'
import { resolveHandoffOrigin } from './handoff-origin'
import { useDeclaredPageSubject, useHasPageSubject } from './page-subject'

/** Days a key minted by the browser sign-in lasts (`CLI_KEY_TTL_DAYS`). */
const SIGN_IN_KEY_DAYS = 90

/**
 * The header's "Hand off to agent" control. It appears on every page that
 * has declared what it is showing (`usePageSubject`) and needs no
 * configuration: no AI provider, no key. It copies a prompt; the person's
 * own agent does the rest.
 *
 * It re-renders when a subject appears or goes away, not when one changes:
 * only the open panel follows the subject itself.
 */
export function AgentHandoffButton() {
  const hasSubject = useHasPageSubject()
  const { pathname } = useLocation()
  const [open, setOpen] = useState(false)
  // The request belongs to the page it was typed on: it survives closing the
  // panel, and does not follow the person to another record.
  const [draft, setDraft] = useState({ pathname, text: '' })
  const request = draft.pathname === pathname ? draft.text : ''

  return (
    <Popover open={open && hasSubject} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          // Mounted on every page so its state is stable; shown only where
          // there is something to hand off.
          className={cn('h-9 gap-2', !hasSubject && 'hidden')}
          title={HAND_OFF_LABEL}
        >
          <SquareTerminal className="h-4 w-4" aria-hidden="true" />
          <span className="max-sm:sr-only">{HAND_OFF_LABEL}</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="w-[34rem] max-w-[calc(100vw-1rem)] space-y-4"
      >
        <AgentHandoffPanel
          request={request}
          onRequestChange={(text) => setDraft({ pathname, text })}
        />
      </PopoverContent>
    </Popover>
  )
}

interface AgentHandoffPanelProps {
  request: string
  onRequestChange: (value: string) => void
}

/** The panel's content. Mounted only while the panel is open. */
export function AgentHandoffPanel({
  request,
  onRequestChange,
}: AgentHandoffPanelProps) {
  const subject = useDeclaredPageSubject()
  const { user } = useAuth()
  const requestId = useId()
  const promptId = useId()
  // Read when the panel opens, not on every page: someone without
  // `settings:read` would otherwise pay a refused request per navigation.
  const settings = useQuery({
    ...getSettingsOptions(),
    retry: false,
    staleTime: 5 * 60_000,
  })
  const resolved = resolveHandoffOrigin({
    externalUrl: settings.data?.external_url,
    settled: settings.isSuccess || settings.isError,
    browserOrigin: window.location.origin,
  })
  const prompt =
    subject && resolved
      ? buildAgentHandoffPrompt({
          origin: resolved.origin,
          cliVersion: TEMPS_CLI_VERSION,
          subject,
          request,
        })
      : null

  return (
    <>
      <h2 className="text-base font-semibold">{HAND_OFF_LABEL}</h2>

      <div className="space-y-2">
        <Label htmlFor={requestId}>What do you want the agent to do?</Label>
        <Input
          id={requestId}
          value={request}
          maxLength={MAX_REQUEST_LENGTH}
          placeholder="Explain what went wrong here"
          onChange={(event) => onRequestChange(event.target.value)}
        />
      </div>

      <div className="space-y-2">
        <p id={promptId} className="text-sm font-medium leading-none">
          Prompt
        </p>
        {prompt ? (
          <pre
            role="region"
            aria-labelledby={promptId}
            tabIndex={0}
            data-testid="agent-handoff-prompt"
            className="max-h-64 overflow-y-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs [overflow-wrap:anywhere] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {prompt}
          </pre>
        ) : (
          <div
            role="status"
            aria-labelledby={promptId}
            aria-busy="true"
            className="space-y-2 rounded-md border bg-muted/40 p-3"
          >
            {['w-3/4', 'w-1/2', 'w-5/6', 'w-2/3', 'w-3/5', 'w-4/5'].map(
              (width) => (
                <Skeleton key={width} className={cn('h-3', width)} />
              )
            )}
          </div>
        )}
        {resolved ? (
          <p className="text-xs text-muted-foreground">
            {resolved.source === 'external_url'
              ? "The address is this instance's external URL setting."
              : "The address is this browser tab's. If your agent runs on another machine, check that it can reach it."}
          </p>
        ) : null}
      </div>

      <div className="flex gap-3 rounded-md bg-muted p-3 text-sm">
        <ShieldAlert
          className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <p>
          The agent signs in as you. Once you approve its sign-in link it holds
          a key with your whole role{user?.role ? ` (${user.role})` : ''} for{' '}
          {SIGN_IN_KEY_DAYS} days, or until you revoke it under{' '}
          <Link to="/settings/keys" className="underline">
            API keys
          </Link>
          . It can do anything you can do here. The rules in the prompt are
          instructions to the agent; this server does not enforce them.
        </p>
      </div>

      <div className="flex justify-end">
        <CopyButton
          value={prompt ?? ''}
          disabled={!prompt}
          label="Copy prompt"
          className="h-9 rounded-md bg-primary px-4 text-primary-foreground hover:bg-primary/90 hover:text-primary-foreground"
        >
          Copy prompt
        </CopyButton>
      </div>
    </>
  )
}
