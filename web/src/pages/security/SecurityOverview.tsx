import {
  getLatestScansPerEnvironmentOptions,
  getEnvironmentsOptions,
  triggerScanMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, ScanResponse } from '@/api/client'
import { VulnerabilityScanCard } from '@/components/vulnerabilities/VulnerabilityScanCard'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Progress } from '@/components/ui/progress'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Shield, AlertTriangle, CheckCircle2, Play, Loader2, Clock } from 'lucide-react'
import { useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { useEffect } from 'react'

interface SecurityOverviewProps {
  project: ProjectResponse
}

function isScanInProgress(scan: ScanResponse | undefined): boolean {
  if (!scan) return false
  return scan.status === 'running' || scan.status === 'pending'
}

function getScanStatusBadge(scan: ScanResponse | undefined) {
  if (!scan) return null

  if (scan.status === 'running' || scan.status === 'pending') {
    return (
      <Badge variant="outline" className="bg-blue-500/10 text-blue-500 border-blue-500/20">
        <Loader2 className="h-3 w-3 mr-1 animate-spin" />
        Scanning...
      </Badge>
    )
  }

  if (scan.status === 'failed') {
    return (
      <Badge variant="outline" className="bg-red-500/10 text-red-500 border-red-500/20">
        <AlertTriangle className="h-3 w-3 mr-1" />
        Failed
      </Badge>
    )
  }

  return null
}

function getVulnerabilitySeverityBadge(scan: ScanResponse | undefined) {
  if (!scan) return null

  const total =
    scan.critical_count + scan.high_count + scan.medium_count + scan.low_count

  if (total === 0) {
    return (
      <Badge variant="outline" className="bg-green-500/10 text-green-500 border-green-500/20">
        <CheckCircle2 className="h-3 w-3 mr-1" />
        Clean
      </Badge>
    )
  }

  if (scan.critical_count > 0) {
    return (
      <Badge variant="outline" className="bg-red-500/10 text-red-500 border-red-500/20">
        <AlertTriangle className="h-3 w-3 mr-1" />
        {scan.critical_count} Critical
      </Badge>
    )
  }

  if (scan.high_count > 0) {
    return (
      <Badge variant="outline" className="bg-orange-500/10 text-orange-500 border-orange-500/20">
        <AlertTriangle className="h-3 w-3 mr-1" />
        {scan.high_count} High
      </Badge>
    )
  }

  return (
    <Badge variant="outline" className="bg-yellow-500/10 text-yellow-500 border-yellow-500/20">
      {total} Vulnerabilities
    </Badge>
  )
}

export function SecurityOverview({ project }: SecurityOverviewProps) {
  const { slug } = useParams<{ slug: string }>()
  const queryClient = useQueryClient()

  // Fetch environments
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Fetch latest scans per environment with polling if any scan is in progress
  const { data: scans, isLoading: isLoadingScans } = useQuery({
    ...getLatestScansPerEnvironmentOptions({
      path: {
        project_id: project.id,
      },
    }),
    refetchInterval: (query) => {
      // Poll every 3 seconds if any scan is in progress
      const hasRunningScan = query.state.data?.some(isScanInProgress)
      return hasRunningScan ? 3000 : false
    },
  })

  // Show toast when scan completes
  useEffect(() => {
    if (!scans) return

    scans.forEach((scan) => {
      if (scan.status === 'completed' && scan.completed_at) {
        const completedAt = new Date(scan.completed_at)
        const now = new Date()
        const secondsSinceComplete = (now.getTime() - completedAt.getTime()) / 1000

        // Only show toast if scan completed in the last 10 seconds (recently completed)
        if (secondsSinceComplete < 10) {
          const total = scan.critical_count + scan.high_count + scan.medium_count + scan.low_count
          if (total > 0) {
            toast.warning(`Scan #${scan.id} completed with ${total} vulnerabilities found`, {
              description: `${scan.critical_count} critical, ${scan.high_count} high, ${scan.medium_count} medium, ${scan.low_count} low`,
            })
          } else {
            toast.success(`Scan #${scan.id} completed with no vulnerabilities found`)
          }
        }
      }
    })
  }, [scans])

  // Trigger scan mutation
  const triggerScan = useMutation({
    ...triggerScanMutation(),
    onSuccess: (data) => {
      toast.success('Vulnerability scan started', {
        description: `Scan #${data.scan_id} is now running. This may take a few minutes.`,
      })
      // Invalidate scans to refetch
      queryClient.invalidateQueries({
        queryKey: getLatestScansPerEnvironmentOptions({
          path: { project_id: project.id },
        }).queryKey,
      })
    },
    onError: (error: any) => {
      toast.error('Failed to start scan', {
        description: error?.message || 'An error occurred while starting the vulnerability scan',
      })
    },
  })

  const isLoading = isLoadingEnvironments || isLoadingScans

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="space-y-2">
          <Skeleton className="h-8 w-64" />
          <Skeleton className="h-4 w-96" />
        </div>
        <Skeleton className="h-12 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  // Create a map of environment ID to scan
  const scansByEnvironment = new Map<number, ScanResponse>()
  scans?.forEach((scan) => {
    if (scan.environment_id) {
      scansByEnvironment.set(scan.environment_id, scan)
    }
  })

  // No environments case
  if (!environments || environments.length === 0) {
    return (
      <div className="space-y-6">
        <div>
          <div className="flex items-center gap-3 mb-2">
            <Shield className="h-6 w-6 text-muted-foreground" />
            <h1 className="text-2xl font-bold">Security Scans</h1>
          </div>
          <p className="text-muted-foreground">
            Vulnerability scan results for each environment
          </p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Shield className="h-12 w-12 text-muted-foreground mb-4" />
            <h3 className="text-lg font-medium mb-2">No environments configured</h3>
            <p className="text-muted-foreground text-center">
              Create an environment to start running vulnerability scans
            </p>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <div className="flex items-center gap-3 mb-2">
          <Shield className="h-6 w-6 text-muted-foreground" />
          <h1 className="text-2xl font-bold">Security Scans</h1>
        </div>
        <p className="text-muted-foreground">
          Vulnerability scan results for each environment
        </p>
      </div>

      {/* Environment Tabs */}
      <Tabs defaultValue={environments[0]?.id.toString()} className="w-full">
        <TabsList className="w-full justify-start border-b rounded-none h-auto p-0 bg-transparent">
          {environments.map((env) => {
            const scan = scansByEnvironment.get(env.id)
            const statusBadge = getScanStatusBadge(scan)
            const severityBadge = statusBadge ? null : getVulnerabilitySeverityBadge(scan)

            return (
              <TabsTrigger
                key={env.id}
                value={env.id.toString()}
                className="rounded-none border-b-2 border-transparent data-[state=active]:border-primary data-[state=active]:bg-transparent px-4 py-3"
              >
                <span className="flex items-center gap-2">
                  {env.name}
                  {statusBadge || severityBadge}
                </span>
              </TabsTrigger>
            )
          })}
        </TabsList>

        {environments.map((env) => {
          const scan = scansByEnvironment.get(env.id)
          const isScanningThisEnv =
            triggerScan.isPending &&
            triggerScan.variables?.body?.environment_id === env.id

          return (
            <TabsContent key={env.id} value={env.id.toString()} className="mt-6">
              {!scan ? (
                <Card>
                  <CardContent className="flex flex-col items-center justify-center py-12">
                    <Shield className="h-12 w-12 text-muted-foreground mb-4" />
                    <h3 className="text-lg font-medium mb-2">No scans yet</h3>
                    <p className="text-muted-foreground text-center mb-6">
                      Vulnerability scans will appear here after your first deployment to{' '}
                      <span className="font-medium">{env.name}</span>
                    </p>
                    <Button
                      onClick={() => {
                        triggerScan.mutate({
                          path: { project_id: project.id },
                          body: { environment_id: env.id },
                        })
                      }}
                      disabled={triggerScan.isPending}
                    >
                      {isScanningThisEnv ? (
                        <>
                          <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                          Starting Scan...
                        </>
                      ) : (
                        <>
                          <Play className="h-4 w-4 mr-2" />
                          Create Scan
                        </>
                      )}
                    </Button>
                  </CardContent>
                </Card>
              ) : (
                <div className="space-y-4">
                  {/* Show progress alert when scan is running */}
                  {isScanInProgress(scan) && (
                    <Alert className="border-blue-500/20 bg-blue-500/10">
                      <Clock className="h-4 w-4 text-blue-500" />
                      <AlertDescription className="text-blue-600 dark:text-blue-400">
                        <div className="flex items-center justify-between mb-2">
                          <span className="font-medium">Scan #{scan.id} in progress...</span>
                          <Loader2 className="h-4 w-4 animate-spin" />
                        </div>
                        <p className="text-sm text-muted-foreground mb-2">
                          This scan is currently running. Results will appear automatically when complete.
                        </p>
                        <Progress value={undefined} className="h-1" />
                      </AlertDescription>
                    </Alert>
                  )}

                  <div className="flex justify-end">
                    <Button
                      onClick={() => {
                        triggerScan.mutate({
                          path: { project_id: project.id },
                          body: { environment_id: env.id },
                        })
                      }}
                      disabled={triggerScan.isPending || isScanInProgress(scan)}
                      variant="outline"
                    >
                      {isScanningThisEnv ? (
                        <>
                          <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                          Starting Scan...
                        </>
                      ) : (
                        <>
                          <Play className="h-4 w-4 mr-2" />
                          Run New Scan
                        </>
                      )}
                    </Button>
                  </div>

                  {/* Only show scan card if scan is completed or failed */}
                  {!isScanInProgress(scan) && (
                    <div className="max-w-2xl">
                      <VulnerabilityScanCard
                        scan={scan}
                        projectSlug={slug || ''}
                        showEnvironment={false}
                      />
                    </div>
                  )}
                </div>
              )}
            </TabsContent>
          )
        })}
      </Tabs>
    </div>
  )
}
