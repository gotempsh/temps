import {
  getLatestScansPerEnvironmentOptions,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, ScanResponse } from '@/api/client'
import { VulnerabilityScanCard } from '@/components/vulnerabilities/VulnerabilityScanCard'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Badge } from '@/components/ui/badge'
import { useQuery } from '@tanstack/react-query'
import { Shield, AlertTriangle, CheckCircle2 } from 'lucide-react'
import { useParams } from 'react-router-dom'

interface SecurityOverviewProps {
  project: ProjectResponse
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

  // Fetch environments
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Fetch latest scans per environment
  const { data: scans, isLoading: isLoadingScans } = useQuery({
    ...getLatestScansPerEnvironmentOptions({
      path: {
        project_id: project.id,
      },
    }),
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
            return (
              <TabsTrigger
                key={env.id}
                value={env.id.toString()}
                className="rounded-none border-b-2 border-transparent data-[state=active]:border-primary data-[state=active]:bg-transparent px-4 py-3"
              >
                <span className="flex items-center gap-2">
                  {env.name}
                  {getVulnerabilitySeverityBadge(scan)}
                </span>
              </TabsTrigger>
            )
          })}
        </TabsList>

        {environments.map((env) => {
          const scan = scansByEnvironment.get(env.id)

          return (
            <TabsContent key={env.id} value={env.id.toString()} className="mt-6">
              {!scan ? (
                <Card>
                  <CardContent className="flex flex-col items-center justify-center py-12">
                    <Shield className="h-12 w-12 text-muted-foreground mb-4" />
                    <h3 className="text-lg font-medium mb-2">No scans yet</h3>
                    <p className="text-muted-foreground text-center">
                      Vulnerability scans will appear here after your first deployment to{' '}
                      <span className="font-medium">{env.name}</span>
                    </p>
                  </CardContent>
                </Card>
              ) : (
                <div className="max-w-2xl">
                  <VulnerabilityScanCard
                    scan={scan}
                    projectSlug={slug || ''}
                    showEnvironment={false}
                  />
                </div>
              )}
            </TabsContent>
          )
        })}
      </Tabs>
    </div>
  )
}
