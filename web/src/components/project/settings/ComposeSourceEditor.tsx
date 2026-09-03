// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  deployComposeSource,
  getComposeSource,
  saveComposeSource,
} from '@/lib/compose-source-api'
import {
  composeSourceDraftForProject,
  composeSourceExpectedRevision,
  type ComposeSourceDraft,
  updateComposeSourceDraft,
} from '@/lib/compose-source-draft'
import { getErrorMessage } from '@/utils/errorHandling'
import Editor from '@monaco-editor/react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { FileCode2, Loader2, Rocket, Save, ShieldAlert } from 'lucide-react'
import { useTheme } from 'next-themes'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'

const sourceQueryKey = (projectId: number) => ['compose-source', projectId]

export function ComposeSourceEditor({
  project,
  refetchProject,
}: {
  project: ProjectResponse
  refetchProject: () => void
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { resolvedTheme } = useTheme()
  const [draftOverride, setDraftOverride] = useState<ComposeSourceDraft | null>(
    null
  )

  const sourceQuery = useQuery({
    queryKey: sourceQueryKey(project.id),
    queryFn: ({ signal }) => getComposeSource(project.id, signal),
    enabled: project.source_type === 'compose',
    retry: false,
  })
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: project.source_type === 'compose',
  })

  const sourceContent = sourceQuery.data?.content ?? ''
  const activeDraft = composeSourceDraftForProject(draftOverride, project.id)
  const draft = activeDraft?.content ?? sourceContent
  const dirty =
    activeDraft !== null && activeDraft.content !== sourceQuery.data?.content

  const productionEnvironment = useMemo(
    () =>
      environmentsQuery.data?.find(
        (environment) => environment.name.toLowerCase() === 'production'
      ) ??
      environmentsQuery.data?.find((environment) => !environment.is_preview),
    [environmentsQuery.data]
  )

  const saveMutation = useMutation({
    mutationFn: () =>
      saveComposeSource(
        project.id,
        draft,
        composeSourceExpectedRevision(
          draftOverride,
          project.id,
          sourceQuery.data?.revision ?? null
        )
      ),
    onSuccess: async (saved) => {
      queryClient.setQueryData(sourceQueryKey(project.id), saved)
      setDraftOverride((current) =>
        current?.projectId === project.id ? null : current
      )
      await refetchProject()
      toast.success(`Compose revision ${saved.revision} saved`)
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not save Docker Compose YAML')),
  })

  const deployMutation = useMutation({
    mutationFn: async () => {
      if (!productionEnvironment) {
        throw new Error('No production environment is available')
      }
      const source = dirty ? await saveMutation.mutateAsync() : sourceQuery.data
      if (!source) throw new Error('No saved Compose source is available')
      return deployComposeSource(
        project.id,
        productionEnvironment.id,
        source.revision
      )
    },
    onSuccess: (deployment) => {
      toast.success('Compose deployment started')
      navigate(`/projects/${project.slug}/deployments/${deployment.id}`)
    },
    onError: (error) =>
      toast.error(
        getErrorMessage(error, 'Could not deploy Docker Compose source')
      ),
  })

  if (sourceQuery.isLoading) {
    return (
      <Card>
        <CardHeader className="space-y-2">
          <Skeleton className="h-5 w-48" />
          <Skeleton className="h-4 w-full max-w-2xl" />
        </CardHeader>
        <CardContent className="space-y-4">
          <Skeleton className="h-96 w-full rounded-lg" />
          <div className="flex justify-end gap-2">
            <Skeleton className="h-9 w-24" />
            <Skeleton className="h-9 w-32" />
          </div>
        </CardContent>
      </Card>
    )
  }

  if (sourceQuery.error || !sourceQuery.data) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="text-base">Docker Compose source</CardTitle>
          <CardDescription>
            Temps could not load this project’s saved Compose document.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {getErrorMessage(
              sourceQuery.error,
              'No Compose revision exists yet'
            )}
            . Paste a document below to initialize this Temps-owned source.
          </p>
          <div className="overflow-hidden rounded-lg border bg-background dark:bg-zinc-950">
            <Editor
              height="24rem"
              language="yaml"
              theme={resolvedTheme === 'dark' ? 'vs-dark' : 'light'}
              value={draft}
              onChange={(value) =>
                setDraftOverride((current) =>
                  updateComposeSourceDraft(
                    current,
                    project.id,
                    value ?? '',
                    sourceQuery.data?.revision ?? null
                  )
                )
              }
              options={{
                minimap: { enabled: false },
                fontSize: 13,
                lineNumbersMinChars: 3,
                scrollBeyondLastLine: false,
                tabSize: 2,
                wordWrap: 'on',
              }}
            />
          </div>
          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => sourceQuery.refetch()}
            >
              Retry load
            </Button>
            <Button
              size="sm"
              disabled={!draft.trim() || saveMutation.isPending}
              onClick={() => saveMutation.mutate()}
            >
              {saveMutation.isPending ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <Save className="mr-2 size-4" />
              )}
              Save first revision
            </Button>
          </div>
        </CardContent>
      </Card>
    )
  }

  const source = sourceQuery.data
  const isPending = saveMutation.isPending || deployMutation.isPending

  return (
    <Card>
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle className="flex items-center gap-2 text-base">
            <FileCode2 className="size-4" />
            Docker Compose source
          </CardTitle>
          <CardDescription className="mt-1">
            This YAML is owned by Temps. Change an image tag or any Compose
            setting, save a revision, then redeploy it without another upload.
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="outline">Revision {source.revision}</Badge>
          {dirty && <Badge variant="secondary">Unsaved changes</Badge>}
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="overflow-hidden rounded-lg border bg-background dark:bg-zinc-950">
          <Editor
            height="30rem"
            language="yaml"
            theme={resolvedTheme === 'dark' ? 'vs-dark' : 'light'}
            value={draft}
            onChange={(value) =>
              setDraftOverride((current) =>
                updateComposeSourceDraft(
                  current,
                  project.id,
                  value ?? '',
                  source.revision
                )
              )
            }
            options={{
              minimap: { enabled: false },
              fontSize: 13,
              lineNumbersMinChars: 3,
              scrollBeyondLastLine: false,
              tabSize: 2,
              wordWrap: 'on',
            }}
          />
        </div>

        <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center">
          <div className="flex min-w-0 flex-1 items-start gap-2 text-xs text-muted-foreground">
            <ShieldAlert className="mt-0.5 size-3.5 shrink-0" />
            <span>
              Put credentials in environment variables or project secrets, not
              directly in this YAML. Temps validates the document before saving.
            </span>
          </div>
          <div className="flex shrink-0 gap-2">
            <Button
              variant="outline"
              disabled={!dirty || isPending}
              onClick={() => saveMutation.mutate()}
            >
              {saveMutation.isPending ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <Save className="mr-2 size-4" />
              )}
              Save revision
            </Button>
            <Button
              disabled={isPending || !productionEnvironment}
              onClick={() => deployMutation.mutate()}
            >
              {deployMutation.isPending ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <Rocket className="mr-2 size-4" />
              )}
              {dirty ? 'Save & deploy' : 'Redeploy'}
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
