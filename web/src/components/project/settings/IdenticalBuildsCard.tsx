// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useIdenticalBuilds } from '@/hooks/useIdenticalBuilds'
import { problemDetail } from '@/lib/api-problem'
import { describeActivity } from '@/lib/identical-builds'
import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useEffect } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  mode: z.enum(['parallel', 'queue', 'share']),
  scope: z.enum(['fingerprint', 'source']),
})

type FormValues = z.infer<typeof schema>

/**
 * What this project's builds do when another project (or environment) is
 * already building the same image. Keyed by project, so an unsaved choice
 * never carries over to the next project this stays mounted for.
 */
export function IdenticalBuildsCard({ projectId }: { projectId: number }) {
  return <ProjectIdenticalBuilds key={projectId} projectId={projectId} />
}

function ProjectIdenticalBuilds({ projectId }: { projectId: number }) {
  const { query, save } = useIdenticalBuilds(projectId)
  const data = query.data
  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: { mode: 'parallel', scope: 'fingerprint' },
  })
  const { reset } = form

  // Load the saved value once it arrives, and again after every save.
  useEffect(() => {
    if (data) reset({ mode: data.mode, scope: data.scope })
  }, [data, reset])

  const onSubmit = (values: FormValues) =>
    save.mutate(values, {
      onSuccess: (saved) => {
        toast.success(
          saved.mode === 'parallel'
            ? 'Every deployment builds its own image again'
            : 'Identical builds saved. Applies from the next build.'
        )
      },
      onError: (error) =>
        toast.error('Could not save identical builds', {
          description: problemDetail(error, 'Try again.'),
        }),
    })

  const mode = useWatch({ control: form.control, name: 'mode' })
  const activity = data ? describeActivity(data.activity) : null
  const editable = data?.can_edit ?? false

  return (
    <form onSubmit={form.handleSubmit(onSubmit)}>
      <Card id="setting-identical-builds" className="scroll-mt-4">
        <CardHeader>
          <CardTitle>Identical builds</CardTitle>
          <CardDescription>
            When other services build the same repository and path at the same
            commit, build once instead of once per service.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {query.isPending && (
            <div className="space-y-2" aria-busy="true">
              <Skeleton className="h-16 w-full" />
              <Skeleton className="h-16 w-full" />
              <Skeleton className="h-16 w-full" />
            </div>
          )}
          {query.isError && (
            <ErrorAlert
              title="Could not load identical builds"
              description={problemDetail(query.error, 'Try again.')}
              retry={() => void query.refetch()}
            />
          )}
          {data && (
            <>
              <Controller
                control={form.control}
                name="mode"
                render={({ field }) => (
                  <RadioGroup
                    value={field.value}
                    onValueChange={field.onChange}
                    aria-label="Identical build mode"
                    disabled={!editable || save.isPending}
                    className="gap-0 divide-y rounded-lg border"
                  >
                    {data.modes.map((option) => (
                      <div
                        key={option.id}
                        className="flex items-start gap-3 p-3"
                      >
                        <RadioGroupItem
                          id={`identical-build-mode-${option.id}`}
                          value={option.id}
                          className="mt-0.5"
                        />
                        <div className="min-w-0 space-y-0.5">
                          <Label htmlFor={`identical-build-mode-${option.id}`}>
                            {option.label}
                          </Label>
                          <p className="text-sm text-muted-foreground">
                            {option.description}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            Example: {option.example}
                          </p>
                        </div>
                      </div>
                    ))}
                  </RadioGroup>
                )}
              />
              {mode !== 'parallel' && (
                <div className="space-y-2">
                  <p className="text-sm font-medium">Which builds wait</p>
                  <Controller
                    control={form.control}
                    name="scope"
                    render={({ field }) => (
                      <RadioGroup
                        value={field.value}
                        onValueChange={field.onChange}
                        aria-label="Which builds wait for each other"
                        disabled={!editable || save.isPending}
                        className="grid-cols-1 gap-2 md:grid-cols-2"
                      >
                        {data.scopes.map((option) => (
                          <div
                            key={option.id}
                            className="flex items-start gap-3 rounded-lg border p-3"
                          >
                            <RadioGroupItem
                              id={`identical-build-scope-${option.id}`}
                              value={option.id}
                              className="mt-0.5"
                            />
                            <div className="min-w-0 space-y-0.5">
                              <Label
                                htmlFor={`identical-build-scope-${option.id}`}
                              >
                                {option.label}
                              </Label>
                              <p className="text-xs text-muted-foreground">
                                {option.description}
                              </p>
                            </div>
                          </div>
                        ))}
                      </RadioGroup>
                    )}
                  />
                </div>
              )}
              {activity && (
                <p className="text-sm text-muted-foreground" role="status">
                  {activity}
                </p>
              )}
            </>
          )}
        </CardContent>
        <CardFooter>
          <p>
            {editable || !data
              ? `Applies from the next build. A build waits at most ${Math.round((data?.max_wait_seconds ?? 5400) / 60)} minutes.`
              : 'You need projects:write on this service to change it.'}
          </p>
          <Button
            type="submit"
            size="sm"
            disabled={
              !data || !editable || !form.formState.isDirty || save.isPending
            }
          >
            {save.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Save
          </Button>
        </CardFooter>
      </Card>
    </form>
  )
}
