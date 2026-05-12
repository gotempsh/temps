import { ProjectResponse } from '@/api/client'
import LogViewer from '../runtime-logs/log-viewer'
import HistoryLogViewer from '../runtime-logs/history-log-viewer'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { Button } from '@/components/ui/button'
import { FileText, Radio, Rocket, ScrollText } from 'lucide-react'
import {
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router-dom'

interface ProjectRuntimeProps {
  project: ProjectResponse
}

export function ProjectRuntime({ project }: ProjectRuntimeProps) {
  const navigate = useNavigate()
  const { slug } = useParams()
  const [searchParams, setSearchParams] = useSearchParams()

  // Sync tab with ?tab= search param (defaults to "live")
  const activeTab = searchParams.get('tab') === 'history' ? 'history' : 'live'

  const handleTabChange = (value: string) => {
    if (value === 'live') {
      searchParams.delete('tab')
    } else {
      searchParams.set('tab', value)
    }
    setSearchParams(searchParams, { replace: true })
  }

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

  return (
    <Tabs value={activeTab} onValueChange={handleTabChange} className="w-full">
      <div className="mb-2">
        <TabsList>
          <TabsTrigger value="live" className="gap-1.5">
            <Radio className="h-3.5 w-3.5" />
            Live
          </TabsTrigger>
          <TabsTrigger value="history" className="gap-1.5">
            <ScrollText className="h-3.5 w-3.5" />
            History
          </TabsTrigger>
        </TabsList>
      </div>
      <TabsContent value="live" className="mt-0">
        <LogViewer project={project} />
      </TabsContent>
      <TabsContent value="history" className="mt-0">
        <HistoryLogViewer project={project} />
      </TabsContent>
    </Tabs>
  )
}
