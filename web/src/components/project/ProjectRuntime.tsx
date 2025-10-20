import { ProjectResponse } from '@/api/client'
import LogViewer from '../runtime-logs/log-viewer'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { FileText, Rocket } from 'lucide-react'
import { useNavigate, useParams } from 'react-router-dom'

interface ProjectRuntimeProps {
  project: ProjectResponse
}

export function ProjectRuntime({ project }: ProjectRuntimeProps) {
  const navigate = useNavigate()
  const { slug } = useParams()

  // Check if project has any deployments
  if (!project.last_deployment) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Card className="w-full max-w-md">
          <CardHeader className="text-center">
            <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-muted">
              <FileText className="h-6 w-6 text-muted-foreground" />
            </div>
            <CardTitle>No Runtime Logs Available</CardTitle>
            <CardDescription>
              Runtime logs will appear here after your first deployment.
            </CardDescription>
          </CardHeader>
          <CardContent className="text-center">
            <Button
              onClick={() => navigate(`/projects/${slug}/deployments`)}
              className="w-full"
            >
              <Rocket className="mr-2 h-4 w-4" />
              Go to Deployments
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  return <LogViewer project={project} />
}
