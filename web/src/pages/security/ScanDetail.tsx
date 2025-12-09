import {
  getScanOptions,
  getScanVulnerabilitiesOptions,
  getEnvironmentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { VulnerabilityList } from '@/components/vulnerabilities/VulnerabilityList'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, Shield } from 'lucide-react'
import { Link, useParams } from 'react-router-dom'

export function ScanDetail() {
  const { slug, scanId } = useParams<{ slug: string; scanId: string }>()

  const { data: scan, isLoading: isScanLoading } = useQuery({
    ...getScanOptions({
      path: {
        scan_id: parseInt(scanId || '0'),
      },
    }),
    enabled: !!scanId,
  })

  const { data: vulnerabilitiesData, isLoading: isVulnerabilitiesLoading } = useQuery({
    ...getScanVulnerabilitiesOptions({
      path: {
        scan_id: parseInt(scanId || '0'),
      },
      query: {
        page: 1,
        page_size: 1000, // Fetch all vulnerabilities
      },
    }),
    enabled: !!scanId,
  })

  // Fetch environment details if available
  const { data: environment } = useQuery({
    ...getEnvironmentOptions({
      path: {
        project_id: scan?.project_id || 0,
        env_id: scan?.environment_id || 0,
      },
    }),
    enabled: !!scan?.project_id && !!scan?.environment_id,
  })

  const vulnerabilities = vulnerabilitiesData?.data || []

  if (isScanLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-10 w-40" />
        <Skeleton className="h-64 w-full max-w-2xl" />
        <Skeleton className="h-96 w-full" />
      </div>
    )
  }

  if (!scan) {
    return (
      <div className="space-y-6">
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Shield className="h-12 w-12 text-muted-foreground mb-4" />
            <h3 className="text-lg font-medium mb-2">Scan not found</h3>
            <p className="text-muted-foreground mb-4">
              The requested vulnerability scan could not be found
            </p>
            <Button variant="outline" asChild>
              <Link to={`/projects/${slug}/security`}>
                <ArrowLeft className="h-4 w-4 mr-2" />
                Back to Security
              </Link>
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  const totalVulnerabilities =
    scan.critical_count + scan.high_count + scan.medium_count + scan.low_count

  return (
    <div className="space-y-6">
      {/* Back Button */}
      <div>
        <Button variant="ghost" size="sm" asChild className="pl-0">
          <Link to={`/projects/${slug}/security`}>
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back to Security
          </Link>
        </Button>
      </div>

      {/* Compact Scan Header */}
      <div className="flex items-start justify-between gap-4 pb-4 border-b">
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Shield className="h-5 w-5 text-muted-foreground" />
            <h1 className="text-xl font-semibold">Vulnerability Scan</h1>
          </div>
          <div className="flex items-center gap-4 text-sm text-muted-foreground">
            {environment && <span>Environment: {environment.name}</span>}
            <span>Scanner: {scan.scanner_type}</span>
            {scan.scanner_version && <span>v{scan.scanner_version}</span>}
            {scan.branch && <span>Branch: {scan.branch}</span>}
            {scan.commit_hash && (
              <span className="font-mono">{scan.commit_hash.substring(0, 7)}</span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2 flex-wrap justify-end">
          {scan.critical_count > 0 && (
            <Badge variant="outline" className="bg-red-500/10 text-red-500 border-red-500/20">
              {scan.critical_count} Critical
            </Badge>
          )}
          {scan.high_count > 0 && (
            <Badge variant="outline" className="bg-orange-500/10 text-orange-500 border-orange-500/20">
              {scan.high_count} High
            </Badge>
          )}
          {scan.medium_count > 0 && (
            <Badge variant="outline" className="bg-yellow-500/10 text-yellow-500 border-yellow-500/20">
              {scan.medium_count} Medium
            </Badge>
          )}
          {scan.low_count > 0 && (
            <Badge variant="outline" className="bg-blue-500/10 text-blue-500 border-blue-500/20">
              {scan.low_count} Low
            </Badge>
          )}
          {totalVulnerabilities === 0 && (
            <Badge variant="outline" className="bg-green-500/10 text-green-500 border-green-500/20">
              Clean
            </Badge>
          )}
        </div>
      </div>

      {/* Vulnerabilities Section */}
      {scan.status === 'completed' && totalVulnerabilities > 0 && (
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <h2 className="text-xl font-semibold">Vulnerabilities</h2>
            <p className="text-sm text-muted-foreground">
              {vulnerabilities.length} {vulnerabilities.length === 1 ? 'vulnerability' : 'vulnerabilities'} found
            </p>
          </div>
          <VulnerabilityList
            vulnerabilities={vulnerabilities}
            isLoading={isVulnerabilitiesLoading}
            scanId={scan.id}
            projectSlug={slug || ''}
          />
        </div>
      )}

      {/* No vulnerabilities message */}
      {scan.status === 'completed' && totalVulnerabilities === 0 && (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Shield className="h-12 w-12 text-green-500 mb-4" />
            <h3 className="text-lg font-medium mb-2">No vulnerabilities found</h3>
            <p className="text-muted-foreground text-center">
              This scan completed successfully with no security vulnerabilities detected
            </p>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
