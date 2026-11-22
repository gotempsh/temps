// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client/types.gen'
import { FunnelForm, FunnelFormData } from '@/components/funnel/FunnelForm'
import { createFunnelMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { Button } from '@/components/ui/button'
import * as React from 'react'
import { getErrorMessage } from '@/utils/errorHandling'
import { useNavigate } from 'react-router'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

/** How long the saved message shows before the funnel opens. */
const FUNNEL_SAVED_REDIRECT_DELAY_MS = 1500

interface CreateFunnelProps {
  project: ProjectResponse
}

export function CreateFunnel({ project }: CreateFunnelProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [feedback, setFeedback] = React.useState<{
    type: 'success' | 'error'
    message: string
  } | null>(null)

  usePageTitle(`Create Funnel - ${project.slug}`)

  React.useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}` },
      { label: 'Analytics', href: `/projects/${project.slug}/analytics` },
      { label: 'Create funnel' },
    ])
  }, [project, setBreadcrumbs])

  const createFunnel = useMutation({
    ...createFunnelMutation(),
    meta: {
      errorTitle: 'Failed to create funnel',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listFunnels'] })
      setFeedback({ type: 'success', message: 'Funnel created successfully!' })
    },
    onError: (error) => {
      setFeedback({
        type: 'error',
        message: getErrorMessage(
          error,
          'Failed to create funnel. Please try again.'
        ),
      })
    },
  })

  // The success message stays up briefly before opening the new funnel. The
  // form stays disabled until then: a second click used to create a duplicate.
  const createdFunnelId = createFunnel.data?.funnel_id
  React.useEffect(() => {
    if (createdFunnelId === undefined) return
    const timer = setTimeout(() => {
      navigate(`/projects/${project.slug}/analytics/funnels/${createdFunnelId}`)
    }, FUNNEL_SAVED_REDIRECT_DELAY_MS)
    return () => clearTimeout(timer)
  }, [createdFunnelId, navigate, project.slug])

  const handleSubmit = (formData: FunnelFormData) => {
    if (
      !formData.name.trim() ||
      !formData.steps.some((step) => step.event_name.trim())
    ) {
      setFeedback({
        type: 'error',
        message: 'Please provide a funnel name and at least one step.',
      })
      return
    }

    const validSteps = formData.steps
      .filter((step) => step.event_name.trim())
      .map(({ showFilters: _, ...step }) => step)

    createFunnel.mutate({
      path: {
        project_id: project.id,
      },
      body: {
        name: formData.name.trim(),
        description: formData.description.trim() || undefined,
        steps: validSteps,
      },
    })
  }

  const handleCancel = () => {
    navigate(`/projects/${project.slug}/analytics`)
  }

  return (
    <div className="relative">
      <div className="absolute top-0 left-0 p-6">
        <Button
          aria-label="Back"
          variant="ghost"
          size="icon"
          onClick={() => navigate(`/projects/${project.slug}/analytics`)}
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
      </div>
      <FunnelForm
        project={project}
        isSubmitting={createFunnel.isPending || createFunnel.isSuccess}
        feedback={feedback}
        onSubmit={handleSubmit}
        onCancel={handleCancel}
        submitLabel="Create Funnel"
        title="Create funnel"
        description="Define conversion steps and track how users progress through your application"
      />
    </div>
  )
}
