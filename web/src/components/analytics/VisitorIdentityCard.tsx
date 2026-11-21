// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Visitor identity on the analytics setup page (ADR 109 D3): stored ids or
 * cookieless, with what each costs and what the tracker must be told.
 */

import {
  getAnalyticsIdentitySettingsOptions,
  updateAnalyticsIdentitySettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  AnalyticsIdentityMode,
  ProjectResponse,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  IDENTITY_MODE_OPTIONS,
  cookielessFactLine,
  cookielessSnippets,
} from '@/lib/visitor-identity'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export function VisitorIdentityCard({ project }: { project: ProjectResponse }) {
  const queryClient = useQueryClient()
  const options = getAnalyticsIdentitySettingsOptions({
    path: { project_id: project.id },
  })
  const settings = useQuery(options)
  // The user's unsaved choice; `null` means "whatever is saved".
  const [choice, setChoice] = useState<AnalyticsIdentityMode | null>(null)
  const saved = settings.data?.mode
  const selected = choice ?? saved ?? 'persistent'

  const update = useMutation({
    ...updateAnalyticsIdentitySettingsMutation(),
    onSuccess: async (result) => {
      toast.success(
        result.mode === 'cookieless'
          ? 'Cookieless tracking is on. Add the cookieless option to the tracker too.'
          : 'Returning visitors are recognised again from their next visit.'
      )
      setChoice(null)
      await queryClient.invalidateQueries({ queryKey: options.queryKey })
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not change the visitor identity mode')),
  })

  const snippets = cookielessSnippets()

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <ShieldCheck className="h-4 w-4" aria-hidden="true" />
          Visitor identity
        </CardTitle>
        <CardDescription>
          Whether Temps stores an id on the visitor&apos;s device. Changing it
          applies to new visits; history already recorded stays as it is.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {settings.isLoading ? (
          <Skeleton className="h-28 w-full" />
        ) : settings.isError ? (
          <p className="text-sm text-muted-foreground">
            {problemDetail(settings.error, 'The identity settings could not be loaded.')}
          </p>
        ) : (
          <RadioGroup
            value={selected}
            onValueChange={(value) => setChoice(value as AnalyticsIdentityMode)}
            className="grid grid-cols-1 gap-3 md:grid-cols-2"
            aria-label="Visitor identity mode"
          >
            {IDENTITY_MODE_OPTIONS.map((option) => {
              const id = `identity-mode-${option.value}`
              return (
                <label
                  key={option.value}
                  htmlFor={id}
                  className="flex cursor-pointer items-start gap-3 rounded-md border p-3 has-[[data-state=checked]]:border-primary"
                >
                  <RadioGroupItem id={id} value={option.value} className="mt-0.5" />
                  <span className="space-y-1">
                    <span className="block text-sm font-medium">
                      {option.title}
                      {saved === option.value && (
                        <span className="ml-2 text-xs font-normal text-muted-foreground">
                          current
                        </span>
                      )}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {option.summary}
                    </span>
                    <ul className="list-disc space-y-0.5 pl-4 text-xs text-muted-foreground">
                      {option.consequences.map((line) => (
                        <li key={line}>{line}</li>
                      ))}
                    </ul>
                  </span>
                </label>
              )
            })}
          </RadioGroup>
        )}

        {selected === 'cookieless' && (
          <div className="space-y-2 text-sm">
            <p className="text-muted-foreground">
              The server stops setting cookies by itself. Tell the tracker as
              well, so it stops writing to <code>localStorage</code>:
            </p>
            <CodeBlock language="tsx" code={snippets.react} showCopy />
            <CodeBlock language="html" code={snippets.script} showCopy />
            <p className="text-xs text-muted-foreground">
              {cookielessFactLine('replays')}
            </p>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            disabled={
              settings.isLoading ||
              settings.isError ||
              selected === saved ||
              update.isPending
            }
            onClick={() =>
              update.mutate({
                path: { project_id: project.id },
                body: { mode: selected },
              })
            }
          >
            {update.isPending ? 'Saving…' : 'Save identity mode'}
          </Button>
          {choice !== null && choice !== saved && (
            <Button size="sm" variant="ghost" onClick={() => setChoice(null)}>
              Cancel
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

/**
 * One line next to a view whose numbers change meaning in cookieless mode.
 * Renders nothing for persistent projects or while the mode is unknown.
 */
export function CookielessFactLine({
  project,
  view,
}: {
  project: ProjectResponse
  view: 'visitors' | 'journey' | 'replays'
}) {
  const settings = useQuery(
    getAnalyticsIdentitySettingsOptions({ path: { project_id: project.id } })
  )
  if (settings.data?.mode !== 'cookieless') return null
  return (
    <p className="text-xs text-muted-foreground" role="note">
      {cookielessFactLine(view)}
    </p>
  )
}
