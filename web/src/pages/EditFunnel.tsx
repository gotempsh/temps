import { ProjectResponse } from '@/api/client/types.gen'
import { FunnelForm, FunnelFormData } from '@/components/funnel/FunnelForm'
import {
  listFunnelsOptions,
  updateFunnelMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription } from '@/components/ui/alert'
import * as React from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

interface EditFunnelProps {
  project: ProjectResponse
}

export function EditFunnel({ project }: EditFunnelProps) {
  const { funnelId } = useParams<{ funnelId: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [feedback, setFeedback] = React.useState<{
    type: 'success' | 'error'
    message: string
  } | null>(null)

  usePageTitle(`Edit Funnel - ${project.name}`)

  // Fetch all funnels to get the specific one
  const { data: funnels, isLoading } = useQuery({
    ...listFunnelsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const funnel = React.useMemo(() => {
    if (!funnels || !funnelId) return null
    return funnels.find((f) => f.id === parseInt(funnelId))
  }, [funnels, funnelId])

  React.useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}` },
      { label: 'Analytics', href: `/projects/${project.slug}/analytics` },
      { label: 'Edit Funnel' },
    ])
  }, [project, setBreadcrumbs])

  const updateFunnel = useMutation({
    ...updateFunnelMutation(),
    meta: {
      errorTitle: 'Failed to update funnel',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listFunnels'] })
      queryClient.invalidateQueries({ queryKey: ['getFunnelMetrics'] })
      setFeedback({ type: 'success', message: 'Funnel updated successfully!' })
      setTimeout(() => {
        navigate(`/projects/${project.slug}/analytics/funnels/${funnelId}`)
      }, 1500)
    },
    onError: (error: any) => {
      setFeedback({
        type: 'error',
        message: error?.message || 'Failed to update funnel. Please try again.',
      })
    },
  })

  const handleSubmit = (formData: FunnelFormData) => {
    if (!formData.name.trim()) {
      setFeedback({ type: 'error', message: 'Please provide a funnel name.' })
      return
    }

    if (!funnelId) {
      setFeedback({ type: 'error', message: 'Funnel ID is missing.' })
      return
    }

    const validSteps = formData.steps
      .filter((step) => step.event_name.trim())
      .map(({ showFilters: _, ...step }) => step)

    updateFunnel.mutate({
      path: {
        project_id: project.id,
        funnel_id: parseInt(funnelId),
      },
      body: {
        name: formData.name.trim(),
        description: formData.description.trim() || undefined,
        steps: validSteps,
      },
    })
  }

  const handleCancel = () => {
    navigate(`/projects/${project.slug}/analytics/funnels/${funnelId}`)
  }

  if (isLoading) {
    return (
      <div className="w-full max-w-7xl mx-auto space-y-6 p-6">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" disabled>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="space-y-2">
            <Skeleton className="h-8 w-48" />
            <Skeleton className="h-4 w-96" />
          </div>
        </div>
        <Card>
          <CardContent className="py-12">
            <div className="space-y-4">
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-20 w-full" />
              <Skeleton className="h-40 w-full" />
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  if (!funnel) {
    return (
      <div className="w-full max-w-7xl mx-auto space-y-6 p-6">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/analytics`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <h1 className="text-3xl font-bold">Funnel Not Found</h1>
        </div>
        <Alert variant="destructive">
          <AlertDescription>
            The requested funnel could not be found. It may have been deleted.
          </AlertDescription>
        </Alert>
      </div>
    )
  }

  // Note: The API doesn't return funnel steps in the list endpoint
  // So we can only edit name and description, not the steps
  const initialData: FunnelFormData = {
    name: funnel.name,
    description: funnel.description || '',
    steps: [
      {
        event_name: 'page_view',
        event_filter: [],
        showFilters: false,
      },
    ],
  }

  return (
    <div className="relative">
      <div className="absolute top-0 left-0 p-6 z-10">
        <Button
          variant="ghost"
          size="icon"
          onClick={() =>
            navigate(`/projects/${project.slug}/analytics/funnels/${funnelId}`)
          }
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
      </div>

      {/* API Limitation Notice */}
      <div className="w-full max-w-7xl mx-auto px-6 pt-6">
        <Alert>
          <AlertDescription>
            <strong>Note:</strong> Currently, you can only edit the funnel name
            and description. To modify funnel steps and filters, please create a
            new funnel.
          </AlertDescription>
        </Alert>
      </div>

      <FunnelForm
        project={project}
        initialData={initialData}
        isSubmitting={updateFunnel.isPending}
        feedback={feedback}
        onSubmit={handleSubmit}
        onCancel={handleCancel}
        submitLabel="Update Funnel"
        title="Edit Funnel"
        description="Update your funnel name and description"
      />
    </div>
  )
}
