import { useState, useEffect } from 'react'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Progress } from '@/components/ui/progress'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  CheckCircle2,
  Circle,
  Loader2,
  AlertCircle,
  Terminal,
  Package,
  Rocket,
  Database,
  Globe,
  PartyPopper,
} from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'

interface DeploymentStep {
  id: string
  name: string
  status: 'pending' | 'running' | 'completed' | 'failed'
  icon: React.ElementType
  logs?: string[]
}

interface DeploymentMonitoringProps {
  projectName: string
  services: string[]
  onComplete: () => void
}

export function DeploymentMonitoring({
  projectName,
  services,
  onComplete,
}: DeploymentMonitoringProps) {
  const [steps, setSteps] = useState<DeploymentStep[]>([
    {
      id: 'clone',
      name: 'Cloning repository',
      status: 'pending',
      icon: Package,
      logs: [],
    },
    {
      id: 'services',
      name: 'Provisioning services',
      status: 'pending',
      icon: Database,
      logs: [],
    },
    {
      id: 'build',
      name: 'Building application',
      status: 'pending',
      icon: Terminal,
      logs: [],
    },
    {
      id: 'deploy',
      name: 'Deploying to container',
      status: 'pending',
      icon: Rocket,
      logs: [],
    },
    {
      id: 'dns',
      name: 'Configuring DNS',
      status: 'pending',
      icon: Globe,
      logs: [],
    },
  ])

  const [currentStepIndex, setCurrentStepIndex] = useState(0)
  const [isComplete, setIsComplete] = useState(false)
  const [showLogs, setShowLogs] = useState(false)
  const [deploymentUrl, setDeploymentUrl] = useState<string>('')

  // Simulate deployment progress
  useEffect(() => {
    if (currentStepIndex >= steps.length) {
      setIsComplete(true)
      setDeploymentUrl(
        `https://${projectName.toLowerCase().replace(/\s+/g, '-')}.temps.app`
      )
      return
    }

    const timer = setTimeout(() => {
      setSteps((prev) => {
        const newSteps = [...prev]

        // Mark current step as completed
        if (
          currentStepIndex < newSteps.length &&
          newSteps[currentStepIndex].status === 'pending'
        ) {
          newSteps[currentStepIndex] = {
            ...newSteps[currentStepIndex],
            status: 'running',
            logs: [
              `Starting ${newSteps[currentStepIndex].name.toLowerCase()}...`,
              'Processing...',
            ],
          }
          return newSteps
        }

        // Mark current step as completed and move to next
        if (
          currentStepIndex < newSteps.length &&
          newSteps[currentStepIndex].status === 'running'
        ) {
          newSteps[currentStepIndex] = {
            ...newSteps[currentStepIndex],
            status: 'completed',
            logs: [
              ...(newSteps[currentStepIndex].logs || []),
              `✓ ${newSteps[currentStepIndex].name} completed successfully`,
            ],
          }
          setCurrentStepIndex((prev) => prev + 1)
        }

        return newSteps
      })
    }, 2000)

    return () => clearTimeout(timer)
  }, [currentStepIndex, steps.length, projectName])

  const progress =
    (steps.filter((s) => s.status === 'completed').length / steps.length) * 100

  if (isComplete) {
    return (
      <div className="space-y-6">
        <div className="text-center space-y-4">
          <div className="flex justify-center">
            <div className="h-20 w-20 rounded-full bg-green-100 dark:bg-green-900/20 flex items-center justify-center">
              <PartyPopper className="h-10 w-10 text-green-600 dark:text-green-400" />
            </div>
          </div>
          <div>
            <h2 className="text-3xl font-bold">Deployment Complete!</h2>
            <p className="text-muted-foreground mt-2">
              Your project has been successfully deployed
            </p>
          </div>
        </div>

        <Card>
          <CardHeader>
            <CardTitle>Project Details</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="flex items-center justify-between p-3 bg-muted rounded-lg">
              <span className="text-sm font-medium">Project Name</span>
              <span className="text-sm text-muted-foreground">
                {projectName}
              </span>
            </div>
            <div className="flex items-center justify-between p-3 bg-muted rounded-lg">
              <span className="text-sm font-medium">Deployment URL</span>
              <a
                href={deploymentUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="text-sm text-primary hover:underline"
              >
                {deploymentUrl}
              </a>
            </div>
            {services.length > 0 && (
              <div className="flex items-center justify-between p-3 bg-muted rounded-lg">
                <span className="text-sm font-medium">Services</span>
                <div className="flex gap-2">
                  {services.map((service) => (
                    <Badge key={service} variant="secondary">
                      {service}
                    </Badge>
                  ))}
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        <div className="flex justify-center gap-4">
          <Button variant="outline" asChild>
            <a href={deploymentUrl} target="_blank" rel="noopener noreferrer">
              <Globe className="mr-2 h-4 w-4" />
              View Deployment
            </a>
          </Button>
          <Button onClick={onComplete}>Go to Project Dashboard</Button>
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold">Deploying Your Project</h2>
        <p className="text-muted-foreground mt-2">
          Sit back while we set everything up for you
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Deployment Progress</CardTitle>
          <CardDescription>Setting up {projectName}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-2">
            <div className="flex items-center justify-between text-sm">
              <span className="text-muted-foreground">Overall Progress</span>
              <span className="font-medium">{Math.round(progress)}%</span>
            </div>
            <Progress value={progress} className="h-2" />
          </div>

          <div className="space-y-3 mt-6">
            {steps.map((step) => {
              const Icon = step.icon
              return (
                <div key={step.id} className="flex items-start gap-3">
                  <div className="mt-0.5">
                    {step.status === 'completed' ? (
                      <CheckCircle2 className="h-5 w-5 text-green-600 dark:text-green-400" />
                    ) : step.status === 'running' ? (
                      <Loader2 className="h-5 w-5 text-primary animate-spin" />
                    ) : step.status === 'failed' ? (
                      <AlertCircle className="h-5 w-5 text-destructive" />
                    ) : (
                      <Circle className="h-5 w-5 text-muted-foreground" />
                    )}
                  </div>
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <Icon className="h-4 w-4 text-muted-foreground" />
                      <span
                        className={`text-sm font-medium ${
                          step.status === 'completed'
                            ? 'text-foreground'
                            : step.status === 'running'
                              ? 'text-primary'
                              : 'text-muted-foreground'
                        }`}
                      >
                        {step.name}
                      </span>
                      {step.status === 'running' && (
                        <Badge variant="secondary" className="text-xs">
                          In Progress
                        </Badge>
                      )}
                    </div>
                    {step.status === 'running' &&
                      step.logs &&
                      step.logs.length > 0 && (
                        <div className="mt-2 text-xs text-muted-foreground">
                          {step.logs[step.logs.length - 1]}
                        </div>
                      )}
                  </div>
                </div>
              )
            })}
          </div>

          {currentStepIndex > 0 && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => setShowLogs(!showLogs)}
              className="w-full"
            >
              <Terminal className="mr-2 h-4 w-4" />
              {showLogs ? 'Hide' : 'Show'} Deployment Logs
            </Button>
          )}

          {showLogs && (
            <Card className="bg-black/5 dark:bg-white/5">
              <CardContent className="p-4">
                <pre className="text-xs font-mono text-muted-foreground whitespace-pre-wrap">
                  {steps
                    .filter((s) => s.logs && s.logs.length > 0)
                    .flatMap((s) => s.logs)
                    .join('\n')}
                </pre>
              </CardContent>
            </Card>
          )}
        </CardContent>
      </Card>

      <Alert>
        <AlertDescription>
          This typically takes 2-5 minutes. You'll be notified when your
          deployment is ready.
        </AlertDescription>
      </Alert>
    </div>
  )
}
