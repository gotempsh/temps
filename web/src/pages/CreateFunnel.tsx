import { ProjectResponse } from '@/api/client/types.gen'
import { FunnelForm, FunnelFormData } from '@/components/funnel/FunnelForm'
import { createFunnelMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { Button } from '@/components/ui/button'
import * as React from 'react'
import { useNavigate } from 'react-router-dom'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

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

  usePageTitle(`Create Funnel - ${project.name}`)

  React.useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}` },
      { label: 'Analytics', href: `/projects/${project.slug}/analytics` },
      { label: 'Create Funnel' },
    ])
  }, [project, setBreadcrumbs])

  const createFunnel = useMutation({
    ...createFunnelMutation(),
    meta: {
      errorTitle: 'Failed to create funnel',
    },
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['listFunnels'] })
      setFeedback({ type: 'success', message: 'Funnel created successfully!' })
      setTimeout(() => {
        navigate(
          `/projects/${project.slug}/analytics/funnels/${data.funnel_id}`
        )
      }, 1500)
    },
    onError: (error: any) => {
      setFeedback({
        type: 'error',
        message: error?.message || 'Failed to create funnel. Please try again.',
      })
    },
  })

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
      .map(({ showFilters, ...step }) => step)

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
          variant="ghost"
          size="icon"
          onClick={() => navigate(`/projects/${project.slug}/analytics`)}
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
      </div>
      <FunnelForm
        project={project}
        isSubmitting={createFunnel.isPending}
        feedback={feedback}
        onSubmit={handleSubmit}
        onCancel={handleCancel}
        submitLabel="Create Funnel"
        title="Create Funnel"
        description="Define conversion steps and track how users progress through your application"
      />
    </div>
  )
}
