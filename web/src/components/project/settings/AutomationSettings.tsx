// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SettingsGroup, StatusDot } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MessageSquare } from 'lucide-react'
import { nanoid } from 'nanoid'
import type { ReactNode } from 'react'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import type { ProjectResponse } from '@/api/client'
import {
  getAutofixCapabilityOptions,
  getAutofixCapabilityQueryKey,
  listAgentsOptions,
  listAgentsQueryKey,
  listAllRunsOptions,
  listConversationsOptions,
  setAutofixPausedMutation,
  updateAgentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  AgentConfigResponse,
  AgentRunResponse,
} from '@/api/client/types.gen'
import { autopilotPaths } from '@/components/agents/AutopilotPage'
import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { AutofixSettingsCard } from '@/components/autofix/AutofixSettingsCard'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  AUTOMATION_TEMPLATES,
  type AutomationTemplate,
  autofixTriggerLabel,
  chatHref,
  describeWorkflowTriggers,
  recentProjectChats,
} from '@/lib/project-automations'
import {
  SETTINGS_SECTION_PARAM,
  settingsSectionDomId,
} from '@/lib/project-settings-sections'
import { getErrorMessage } from '@/utils/errorHandling'

/**
 * Settings → Automation: jobs the AI does on its own, each run being a chat
 * in this project. Groups, top to bottom: what is set up, prompts to start
 * from, the chats they produced, and crash autofix's policy.
 */
export function AutomationSettings({ project }: { project: ProjectResponse }) {
  return (
    <div className="divide-y [&>section]:py-8 [&>section:first-of-type]:pt-0">
      <SettingsGroup
        title="Automations"
        description="Each one gives the AI a job and a moment to do it. Every run is a chat in this service: open it to see what it found and did, or reply to keep it going."
      >
        <AutomationList project={project} />
      </SettingsGroup>
      <SettingsGroup
        id={settingsSectionDomId('templates')}
        title="Start from a prompt"
        description="Ready-made jobs for this service. Try one in a chat to see what it does before it runs on its own."
      >
        <div className="divide-y">
          {AUTOMATION_TEMPLATES.map((template) => (
            <TemplateRow
              key={template.id}
              project={project}
              template={template}
            />
          ))}
        </div>
      </SettingsGroup>
      <SettingsGroup
        id={settingsSectionDomId('chats')}
        title="Recent chats"
        description="What automations and your team asked the AI about this service, most recent first."
      >
        <RecentChats projectId={project.id} />
      </SettingsGroup>
      <SettingsGroup
        id={settingsSectionDomId('crash-autofix')}
        title="Crash autofix"
        description="When a crash starts a fix on its own, and what a fix has to pass before it is offered to you."
      >
        <AutofixSettingsCard projectId={project.id} showPause={false} />
      </SettingsGroup>
    </div>
  )
}

function AutomationRow({
  id,
  name,
  when,
  details,
  enabled,
  onEnabledChange,
  pending,
}: {
  id: string
  name: string
  when: string
  details?: ReactNode
  enabled: boolean
  onEnabledChange: (enabled: boolean) => void
  pending: boolean
}) {
  return (
    <div className="flex items-start justify-between gap-6 py-4 first:pt-0 last:pb-0">
      <div className="min-w-0 space-y-1">
        <Label htmlFor={id} className="text-sm font-medium">
          {name}
        </Label>
        <p className="text-sm text-muted-foreground">
          <span className="font-medium text-foreground">When:</span> {when}
        </p>
        {details && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
            {details}
          </div>
        )}
      </div>
      <Switch
        id={id}
        checked={enabled}
        onCheckedChange={onEnabledChange}
        disabled={pending}
        aria-label={`Run ${name} automatically`}
        className="mt-0.5 shrink-0"
      />
    </div>
  )
}

function AutomationList({ project }: { project: ProjectResponse }) {
  const queryClient = useQueryClient()
  const paths = autopilotPaths(project.slug)
  const path = { project_id: project.id }

  const autofix = useQuery(getAutofixCapabilityOptions({ path }))
  const agents = useQuery(listAgentsOptions({ path }))
  const workflows = (agents.data?.items ?? []) as AgentConfigResponse[]
  const runs = useQuery({
    ...listAllRunsOptions({ path }),
    enabled: workflows.length > 0,
  })

  const pause = useMutation({
    ...setAutofixPausedMutation(),
    onSuccess: (settings) => {
      toast.success(
        settings.paused ? 'Crash autofix paused' : 'Crash autofix on'
      )
      queryClient.invalidateQueries({
        queryKey: getAutofixCapabilityQueryKey({ path }),
      })
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not change crash autofix')),
  })
  const toggleWorkflow = useMutation({
    ...updateAgentMutation(),
    onSuccess: (_, { body }) => {
      toast.success(body.enabled ? 'Automation on' : 'Automation off')
      queryClient.invalidateQueries({ queryKey: listAgentsQueryKey({ path }) })
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not change the automation')),
  })

  if (autofix.isPending || agents.isPending) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-12 w-full" />
        <Skeleton className="h-12 w-full" />
      </div>
    )
  }

  const cap = autofix.data
  const lastRun = latestRunBySlug(runs.data?.items ?? [])
  // Show the value being saved while the request is in flight. A setting
  // that did not load is shown off, not on.
  const autofixOn = pause.isPending
    ? !pause.variables?.body.paused
    : cap
      ? !cap.settings.paused
      : false

  return (
    <div id="agents" className="divide-y">
      <AutomationRow
        id="automation-crash-autofix"
        name="Crash autofix"
        when={autofixTriggerLabel(cap?.settings.mode)}
        enabled={autofixOn}
        pending={pause.isPending || !cap}
        onEnabledChange={(on) => pause.mutate({ path, body: { paused: !on } })}
        details={
          <>
            {cap && !cap.configured && (
              <span className="text-warning-strong">
                {cap.reason}.{' '}
                {cap.setup_path && (
                  <Link className="underline" to={cap.setup_path}>
                    Set it up
                  </Link>
                )}
              </span>
            )}
            {autofix.isError && (
              <span className="text-destructive">
                {getErrorMessage(autofix.error, 'Could not load crash autofix')}
              </span>
            )}
            <Link
              className="underline"
              to={{ search: `?${SETTINGS_SECTION_PARAM}=crash-autofix` }}
            >
              Change when it runs
            </Link>
          </>
        }
      />
      {workflows.map((workflow) => {
        const triggers = describeWorkflowTriggers(workflow.trigger_config)
        const run = lastRun.get(workflow.slug)
        return (
          <AutomationRow
            key={workflow.id}
            id={`automation-${workflow.slug}`}
            name={workflow.name}
            when={
              triggers.length > 0 ? triggers.join(' · ') : 'Nothing starts it'
            }
            enabled={
              toggleWorkflow.isPending &&
              toggleWorkflow.variables?.body.slug === workflow.slug
                ? !!toggleWorkflow.variables.body.enabled
                : workflow.enabled
            }
            pending={toggleWorkflow.isPending}
            onEnabledChange={(enabled) =>
              toggleWorkflow.mutate({
                path: { project_id: project.id, slug: workflow.slug },
                body: { slug: workflow.slug, enabled },
              })
            }
            details={
              <>
                <Link className="underline" to={paths.editAgent(workflow.slug)}>
                  Edit prompt
                </Link>
                {run && (
                  <Link className="underline" to={paths.run(run.id)}>
                    Last run <TimeAgo date={run.created_at} />
                  </Link>
                )}
              </>
            }
          />
        )
      })}
      {agents.isError && (
        <p className="py-4 text-sm text-destructive">
          {getErrorMessage(agents.error, 'Could not load automations')}
        </p>
      )}
    </div>
  )
}

/** The most recent run of each workflow, from a newest-first run list. */
function latestRunBySlug(runs: readonly AgentRunResponse[]) {
  const latest = new Map<string, AgentRunResponse>()
  for (const run of runs) {
    if (run.agent_slug && !latest.has(run.agent_slug))
      latest.set(run.agent_slug, run)
  }
  return latest
}

function TemplateRow({
  project,
  template,
}: {
  project: ProjectResponse
  template: AutomationTemplate
}) {
  const { openPage } = useAiAssistant()
  const navigate = useNavigate()
  const prompt = template.prompt(project.name)
  // Opens the full chat page, not the overlay: a new automation is worth the
  // whole screen, and the page is where its chats are found again later.
  const tryInChat = () => {
    openPage({
      projectId: project.id,
      context: {
        // A plain project chat: the prompt, not a context type, decides
        // what it does. Each try is a new chat.
        contextType: 'project',
        // `crypto.randomUUID` needs a secure context; a console served over
        // plain HTTP has none.
        contextId: nanoid(),
        title: template.title,
        description: `The "${template.title}" prompt is in the box below. Edit it if you like, then send.`,
        startPrompt: prompt,
        projectSlug: project.slug,
        projectName: project.name,
      },
    })
    navigate('/chat')
  }
  return (
    <div className="flex flex-col gap-3 py-4 first:pt-0 last:pb-0 sm:flex-row sm:items-start sm:justify-between sm:gap-6">
      <div className="min-w-0 space-y-1">
        <p className="text-sm font-medium">{template.title}</p>
        <p className="text-sm text-muted-foreground">
          <span className="font-medium text-foreground">For:</span>{' '}
          {template.when}
        </p>
        <p className="line-clamp-2 text-xs text-muted-foreground">{prompt}</p>
      </div>
      <Button
        size="sm"
        variant="outline"
        className="shrink-0"
        onClick={tryInChat}
      >
        <MessageSquare className="size-4" />
        Try in chat
      </Button>
    </div>
  )
}

function RecentChats({ projectId }: { projectId: number }) {
  const conversations = useQuery(
    listConversationsOptions({ path: { project_id: projectId } })
  )

  if (conversations.isPending) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-5 w-full" />
        <Skeleton className="h-5 w-full" />
        <Skeleton className="h-5 w-2/3" />
      </div>
    )
  }
  if (conversations.isError) {
    return (
      <p className="text-sm text-destructive">
        {getErrorMessage(conversations.error, 'Could not load chats')}
      </p>
    )
  }

  const chats = recentProjectChats(conversations.data)
  if (chats.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No chats yet. Try a prompt above, or turn on an automation, and its chat
        shows up here.
      </p>
    )
  }
  return (
    <ul className="divide-y">
      {chats.map((chat) => (
        <li key={chat.public_id}>
          <Link
            to={chatHref(chat.public_id)}
            className="flex items-center gap-3 py-2.5 text-sm first:pt-0 hover:text-foreground"
          >
            <StatusDot
              tone={chat.turn_status === 'running' ? 'running' : 'idle'}
            />
            <span className="min-w-0 flex-1 truncate font-medium">
              {chat.title || 'Untitled chat'}
            </span>
            <TimeAgo
              date={chat.last_activity_at}
              className="shrink-0 text-xs text-muted-foreground"
            />
          </Link>
        </li>
      ))}
    </ul>
  )
}
