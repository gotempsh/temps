import { getProjectBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { LiveVisitorsList } from '@/components/visitors/LiveVisitorsList'
import { Button } from '@/components/ui/button'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { Link, useParams } from 'react-router-dom'
import { Skeleton } from '@/components/ui/skeleton'

interface LiveVisitorsProps {
  project?: ProjectResponse
}

export function LiveVisitors({ project: projectProp }: LiveVisitorsProps = {}) {
  const { slug } = useParams()

  const { data: queriedProject, isLoading } = useQuery({
    ...getProjectBySlugOptions({
      path: {
        slug: slug || '',
      },
    }),
    enabled: !!slug && !projectProp,
  })

  const project = projectProp || queriedProject

  usePageTitle(`${project?.slug || 'Project'} - Live Visitors`)

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6 space-y-6">
          <Button variant="outline" size="sm" disabled>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Project
          </Button>
          <div className="space-y-4">
            <Skeleton className="h-8 w-48" />
            <Skeleton className="h-4 w-96" />
          </div>
        </div>
      </div>
    )
  }

  if (!project) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6 space-y-6">
          <Button variant="outline" size="sm" asChild>
            <Link to="/projects">
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back to Projects
            </Link>
          </Button>
          <div className="text-center py-12">
            <p className="text-muted-foreground">Project not found</p>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-6 space-y-6">
        <Button variant="outline" size="sm" asChild>
          <Link to={`/projects/${project.slug}`}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Project
          </Link>
        </Button>

        <div>
          <h1 className="text-3xl font-bold">Live Visitors</h1>
          <p className="text-muted-foreground mt-1">
            See who's currently browsing {project.name}
          </p>
        </div>

        <LiveVisitorsList project={project} />
      </div>
    </div>
  )
}
