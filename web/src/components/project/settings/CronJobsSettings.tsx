import {
  getEnvironmentCronsOptions,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Clock, FileCode } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useState, useEffect } from 'react'
import { EmptyState } from '@/components/ui/empty-state'
import { useNavigate } from 'react-router-dom'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

interface CronJobsSettingsProps {
  project: ProjectResponse
}

export function CronJobsSettings({ project }: CronJobsSettingsProps) {
  const [selectedEnvironment, setSelectedEnvironment] = useState<string>('')
  const navigate = useNavigate()
  const [showInstructions, setShowInstructions] = useState(false)

  // Fetch environments
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Set first environment as default when environments are loaded
  useEffect(() => {
    if (environments?.length && !selectedEnvironment) {
      setSelectedEnvironment(environments[0].id.toString())
    }
  }, [environments, selectedEnvironment])

  const { data: crons, isLoading: isLoadingCrons } = useQuery({
    ...getEnvironmentCronsOptions({
      path: {
        project_id: project.id,
        env_id: parseInt(selectedEnvironment) || 0,
      },
    }),
    enabled: !!selectedEnvironment,
  })

  const handleEnvironmentChange = (value: string) => {
    setSelectedEnvironment(value)
  }

  const isLoading = isLoadingEnvironments || isLoadingCrons

  // Create a component for the instructions content to avoid duplication
  const InstructionsContent = () => (
    <div className="space-y-4">
      <div>
        <p className="mb-2">
          Create a{' '}
          <code className="text-xs bg-muted px-1 py-0.5 rounded">
            .temps.yaml
          </code>{' '}
          file in your repository root with your cron jobs configuration:
        </p>
        <pre className="text-xs bg-muted p-4 rounded-md overflow-x-auto">
          {`cron:
  - path: "/api/ping"
    schedule: "0 */5 * * * *"  # Every 5 minutes
    
  - path: "/api/daily-backup"
    schedule: "0 0 0 * * *"    # Daily at midnight
    
  - path: "/api/weekly-report"
    schedule: "0 0 0 * * 0"    # Weekly on Sunday`}
        </pre>
      </div>

      <div className="space-y-2">
        <h4 className="font-medium">Schedule Format</h4>
        <p className="text-sm text-muted-foreground">
          The schedule uses standard cron syntax with 6 fields:
        </p>
        <pre className="text-xs bg-muted p-2 rounded-md">{`second minute hour day month weekday`}</pre>
        <div className="text-sm text-muted-foreground space-y-1">
          <p>Common patterns:</p>
          <ul className="list-disc list-inside space-y-1">
            <li>
              <code className="text-xs bg-muted px-1 py-0.5 rounded">
                0 */5 * * * *
              </code>{' '}
              - Every 5 minutes
            </li>
            <li>
              <code className="text-xs bg-muted px-1 py-0.5 rounded">
                0 0 * * * *
              </code>{' '}
              - Every hour
            </li>
            <li>
              <code className="text-xs bg-muted px-1 py-0.5 rounded">
                0 0 0 * * *
              </code>{' '}
              - Daily at midnight
            </li>
            <li>
              <code className="text-xs bg-muted px-1 py-0.5 rounded">
                0 0 0 * * 0
              </code>{' '}
              - Weekly on Sunday
            </li>
          </ul>
        </div>
      </div>

      <div className="space-y-2">
        <h4 className="font-medium">Notes</h4>
        <ul className="text-sm text-muted-foreground list-disc list-inside space-y-1">
          <li>The path should be a valid endpoint in your application</li>
          <li>The endpoint will be called with a POST request</li>
          <li>Changes will be applied on your next deployment</li>
        </ul>
      </div>
    </div>
  )

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-medium">Cron Jobs</h2>
          <p className="text-sm text-muted-foreground">
            Schedule recurring tasks and automated jobs
          </p>
        </div>
        {crons?.length ? (
          <Button
            disabled={!selectedEnvironment}
            onClick={() => setShowInstructions(true)}
          >
            <FileCode className="h-4 w-4 mr-2" />
            Learn how to add cron jobs
          </Button>
        ) : null}
      </div>

      <Dialog open={showInstructions} onOpenChange={setShowInstructions}>
        <DialogContent className="sm:max-w-[500px]">
          <DialogHeader>
            <DialogTitle>Add Cron Jobs</DialogTitle>
            <DialogDescription>
              Follow these steps to add cron jobs to your project
            </DialogDescription>
          </DialogHeader>
          <InstructionsContent />
        </DialogContent>
      </Dialog>

      <div className="flex items-center gap-2">
        <Select
          value={selectedEnvironment}
          onValueChange={handleEnvironmentChange}
        >
          <SelectTrigger className="w-[200px]" disabled={isLoadingEnvironments}>
            <SelectValue placeholder="Select environment">
              {environments?.find(
                (env) => env.id.toString() === selectedEnvironment
              )?.name || 'Select environment'}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            {environments?.map((env) => (
              <SelectItem key={env.id} value={env.id.toString()}>
                {env.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {!selectedEnvironment ? (
        <EmptyState
          icon={Clock}
          title="Select an Environment"
          description="Choose an environment to view and manage cron jobs"
        />
      ) : isLoading ? (
        <div className="space-y-4">
          {Array.from({ length: 3 }).map((_, i) => (
            <Card key={i} className="animate-pulse">
              <CardContent className="h-24" />
            </Card>
          ))}
        </div>
      ) : !crons?.length ? (
        <EmptyState
          icon={Clock}
          title="No Cron Jobs"
          description={
            <div className="space-y-2">
              <p>Get started by adding your first cron job</p>
              <Button
                variant="default"
                onClick={() => setShowInstructions(true)}
              >
                View setup instructions
              </Button>
            </div>
          }
        />
      ) : (
        <div className="space-y-4">
          {crons.map((cron) => (
            <Card
              key={cron.id}
              className="cursor-pointer hover:bg-muted/50 transition-colors"
              onClick={() =>
                navigate(
                  `/projects/${project.slug}/settings/cron-jobs/${selectedEnvironment}/${cron.id}`
                )
              }
            >
              <CardHeader className="pb-4">
                <div className="flex items-start justify-between">
                  <div>
                    <CardTitle className="text-base">{cron.path}</CardTitle>
                    <CardDescription className="mt-1">
                      <code className="text-sm">{cron.schedule}</code>
                    </CardDescription>
                  </div>
                  <div className="flex items-center gap-2">
                    <Badge variant="secondary">
                      Next run:{' '}
                      {cron.next_run
                        ? new Date(cron.next_run).toLocaleString()
                        : 'Not scheduled'}
                    </Badge>
                  </div>
                </div>
              </CardHeader>
              <CardContent>
                <div className="text-sm text-muted-foreground">
                  Created: {new Date(cron.created_at).toLocaleString()}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}
