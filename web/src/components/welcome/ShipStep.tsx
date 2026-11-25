// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getAiHarnessStatusOptions,
  getProjectsOptions,
  getProjectsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import { problemDetail } from '@/lib/api-problem'
import { Button, Callout } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useNavigate } from 'react-router'
import { ChoiceList } from './ChoiceList'
import { LATER_REASONS, deployAppPrompt } from './onboarding'
import { useUpdateOnboarding } from './useOnboarding'

type Source = 'agent' | 'project'

const QUESTION = 'What should go live first?'

/**
 * Step 4: your own app live, two ways. The agent connected in step 3 deploys
 * it from a prompt, and the step watches for the project it creates; or the
 * New project page, which already does repositories, templates and images,
 * rather than a second copy of it here.
 */
export function ShipStep() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const updateOnboarding = useUpdateOnboarding()
  const harness = useQuery(getAiHarnessStatusOptions())
  const agentConnected = harness.data?.status === 'connected'
  // Until the user picks, the agent leads when one is connected.
  const [picked, setPicked] = useState<Source | null>(null)
  const source: Source = picked ?? (agentConnected ? 'agent' : 'project')

  const origin = window.location.origin
  const prompt = deployAppPrompt(origin)

  // The projects that existed when the step opened; one that isn't among
  // them is the agent's.
  const [before, setBefore] = useState<Set<number> | null>(null)
  const newProject = (list: ProjectResponse[] | undefined) =>
    (before && list?.find((p) => !before.has(p.id))) || null
  const projects = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 20 } }),
    refetchInterval: (query) =>
      source === 'agent' && !newProject(query.state.data?.projects)
        ? 3000
        : false,
  })
  const list = projects.data?.projects
  // Recorded once, during render: React's pattern for state derived from
  // the first value of something else.
  if (list && before === null) setBefore(new Set(list.map((p) => p.id)))
  const created = newProject(list)

  const finish = async (path: string) => {
    await updateOnboarding.mutateAsync({ done: ['app'], completed: true })
    await queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() })
    navigate(path)
  }

  const openCreated = useMutation({
    mutationFn: async (slug: string) => finish(`/projects/${slug}`),
  })
  // Leaving for New project closes the wizard but leaves the step open:
  // nothing is live until that page deploys.
  const goToNewProject = useMutation({
    mutationFn: () => updateOnboarding.mutateAsync({ completed: true }),
    onSuccess: () => navigate('/projects/new'),
  })
  const later = useMutation({
    mutationFn: () =>
      updateOnboarding.mutateAsync({
        later: [{ step: 'app', reason: LATER_REASONS.app }],
        completed: true,
      }),
    onSuccess: () => navigate('/projects'),
  })

  const pending =
    openCreated.isPending || goToNewProject.isPending || later.isPending
  const error = openCreated.error ?? goToNewProject.error ?? later.error

  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold">{QUESTION}</h2>
      <ChoiceList<Source>
        label={QUESTION}
        columns={2}
        value={source}
        onChange={setPicked}
        choices={[
          {
            id: 'agent',
            label: 'Ask your agent',
            detail:
              'It asks what you want to deploy or build, then creates the project and puts it live. You watch here.',
            badge: agentConnected ? (
              <Badge variant="secondary">Recommended</Badge>
            ) : undefined,
            note: agentConnected
              ? undefined
              : 'No agent is connected yet: the step before connects one.',
          },
          {
            id: 'project',
            label: 'A new project',
            detail:
              'From a repository, a template or an image, on the New project page.',
          },
        ]}
      />

      {source === 'agent' && (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-sm font-medium">Paste this into your agent</p>
            <CopyButton
              value={prompt}
              minimal
              label="Copy prompt"
              className="h-8 rounded-md border px-3"
            >
              Copy prompt
            </CopyButton>
          </div>
          <pre
            role="region"
            aria-label="Prompt for your agent"
            tabIndex={0}
            className="max-h-72 overflow-y-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs [overflow-wrap:anywhere]"
          >
            {prompt}
          </pre>
          {created ? (
            <Callout
              tone="success"
              title={`Your agent created ${created.name}`}
            >
              Open it to follow the deploy.
            </Callout>
          ) : (
            <p
              id="ship-agent-status"
              className="text-sm text-muted-foreground"
              role="status"
            >
              {projects.isError
                ? problemDetail(
                    projects.error,
                    'Could not check for a new project.'
                  )
                : 'Waiting for a new project…'}
            </p>
          )}
        </div>
      )}

      {error ? (
        <Callout tone="error" title="That didn’t work">
          {problemDetail(error, 'Try again.')}
        </Callout>
      ) : null}

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        {source === 'agent' && (
          <Button
            busy={openCreated.isPending}
            busyLabel="Opening…"
            aria-disabled={!created || pending || undefined}
            aria-describedby={created ? undefined : 'ship-agent-status'}
            onClick={() =>
              created && !pending && openCreated.mutate(created.slug)
            }
          >
            {created ? `Open ${created.name}` : 'Open the project'}
          </Button>
        )}
        {source === 'project' && (
          <Button
            busy={goToNewProject.isPending}
            busyLabel="Opening…"
            aria-disabled={pending || undefined}
            onClick={() => !pending && goToNewProject.mutate()}
          >
            Go to New project
          </Button>
        )}
        <Button
          variant="ghost"
          busy={later.isPending}
          busyLabel="Saving…"
          aria-disabled={pending || undefined}
          onClick={() => !pending && later.mutate()}
        >
          Do this later
        </Button>
      </div>
    </div>
  )
}
