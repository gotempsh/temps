// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ProjectResponse } from '@/api/client'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  changeProjectSourceMutation,
  getProjectBySlugQueryKey,
  getProjectsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Link, useNavigate } from 'react-router'
import { PageHeader } from '@/components/layout/PageContainer'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { FolderUp, ArrowRight, Loader2 } from 'lucide-react'

export function AddProjectHosting({ project }: { project: ProjectResponse }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const enable = useMutation({
    ...changeProjectSourceMutation(),
    onSuccess: async (_, variables) => {
      navigate(
        `/projects/${project.slug}/${variables.body.source_type === 'static_files' ? 'drop' : 'project'}`
      )
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: getProjectBySlugQueryKey({ path: { slug: project.slug } }),
        }),
        queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() }),
      ])
    },
  })
  return (
    <section className="space-y-6">
      <PageHeader
        title="Run your app on Temps"
        description="Choose where your code is. Your saved data stays here."
      />
      <ol
        aria-label="Hosting steps"
        className="flex flex-wrap gap-x-6 gap-y-2 text-sm text-muted-foreground"
      >
        <li className="font-medium text-foreground">1. Choose your code</li>
        <li>2. Check the settings</li>
        <li>3. Deploy your app</li>
      </ol>
      <div className="divide-y rounded-lg border bg-card">
        <div className="flex flex-wrap items-center justify-between gap-4 p-4">
          <div>
            <h2 className="flex items-center gap-2 font-medium">
              <ProviderLogo providerType="github" className="size-6" />
              <ProviderLogo providerType="gitlab" className="size-6" />
              From GitHub or GitLab
            </h2>
            <p className="text-sm text-muted-foreground">
              Pick the repo that holds your app’s code.
            </p>
          </div>
          <Button asChild variant="outline">
            <Link to={`/projects/${project.slug}/connect-repository`}>
              Choose a repository <ArrowRight className="size-4" />
            </Link>
          </Button>
        </div>
        {(
          [
            {
              type: 'docker_image',
              title: 'Docker image',
              description:
                'Already have a Docker image? Use it to run your app.',
            },
            {
              type: 'static_files',
              title: 'Website files',
              description:
                'Upload a ready-to-use website as a folder or ZIP file.',
            },
          ] as const
        ).map((source) => (
          <div
            key={source.type}
            className="flex flex-wrap items-center justify-between gap-4 p-4"
          >
            <div>
              <h2 className="flex items-center gap-2 font-medium">
                {source.type === 'docker_image' ? (
                  <img
                    src="/presets/docker.svg"
                    alt="Docker"
                    className="size-6 object-contain"
                  />
                ) : (
                  <FolderUp className="size-6 text-muted-foreground" />
                )}
                {source.title}
              </h2>
              <p className="text-sm text-muted-foreground">
                {source.description}
              </p>
            </div>
            <Button
              variant="outline"
              disabled={enable.isPending}
              onClick={() =>
                enable.mutate({
                  path: { id: project.id },
                  body: { source_type: source.type },
                })
              }
            >
              {enable.isPending &&
              enable.variables?.body.source_type === source.type ? (
                <>
                  <Loader2 className="size-4 animate-spin" /> Opening…
                </>
              ) : (
                <>
                  Continue with{' '}
                  {source.type === 'docker_image' ? 'Docker' : 'files'}{' '}
                  <ArrowRight className="size-4" />
                </>
              )}
            </Button>
          </div>
        ))}
      </div>
      {enable.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            We could not add hosting. Try again. If it still fails, ask your
            admin for help.
          </AlertDescription>
        </Alert>
      )}
      <p className="text-sm text-muted-foreground">
        Your app will not go live yet. You can check the settings before you
        deploy.
      </p>
    </section>
  )
}
