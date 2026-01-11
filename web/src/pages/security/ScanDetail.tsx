import {
  getScanOptions,
  getScanVulnerabilitiesOptions,
  getEnvironmentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { VulnerabilityResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { VulnerabilityList } from '@/components/vulnerabilities/VulnerabilityList'
import { Input } from '@/components/ui/input'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, Shield, Download, FileJson, FileSpreadsheet, Search, Package, Code, Filter } from 'lucide-react'
import { Link, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { useState, useMemo } from 'react'
import Fuse from 'fuse.js'

function exportToJSON(vulnerabilities: VulnerabilityResponse[], scanId: string) {
  const jsonData = JSON.stringify(vulnerabilities, null, 2)
  const blob = new Blob([jsonData], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = `scan-${scanId}-vulnerabilities.json`
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  URL.revokeObjectURL(url)
  toast.success('Vulnerabilities exported as JSON')
}

function exportToCSV(vulnerabilities: VulnerabilityResponse[], scanId: string) {
  // CSV headers
  const headers = [
    'Vulnerability ID',
    'Title',
    'Severity',
    'CVSS Score',
    'Package Name',
    'Installed Version',
    'Fixed Version',
    'Class',
    'Type',
    'Target',
    'Description',
    'Published Date',
    'Last Modified',
    'Primary URL',
  ]

  // Convert vulnerabilities to CSV rows
  const rows = vulnerabilities.map((vuln) => [
    vuln.vulnerability_id,
    vuln.title.replace(/"/g, '""'), // Escape quotes
    vuln.severity,
    vuln.cvss_score?.toString() || '',
    vuln.package_name,
    vuln.installed_version,
    vuln.fixed_version || '',
    vuln.class || '',
    vuln.type || '',
    vuln.target || '',
    vuln.description?.replace(/"/g, '""') || '', // Escape quotes
    vuln.published_date || '',
    vuln.last_modified_date || '',
    vuln.primary_url || '',
  ])

  // Build CSV string
  const csvContent = [
    headers.join(','),
    ...rows.map((row) => row.map((cell) => `"${cell}"`).join(',')),
  ].join('\n')

  const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = `scan-${scanId}-vulnerabilities.csv`
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  URL.revokeObjectURL(url)
  toast.success('Vulnerabilities exported as CSV')
}

export function ScanDetail() {
  const { slug, scanId } = useParams<{ slug: string; scanId: string }>()
  const [searchQuery, setSearchQuery] = useState('')
  const [selectedTypes, setSelectedTypes] = useState<Set<string>>(new Set())
  const [showTypeFilters, setShowTypeFilters] = useState(false)

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

  // Configure Fuse.js for fuzzy search
  const fuse = useMemo(
    () =>
      new Fuse(vulnerabilities, {
        keys: [
          { name: 'vulnerability_id', weight: 2 }, // CVE ID gets higher weight
          { name: 'title', weight: 1.5 },
          { name: 'package_name', weight: 1.5 },
          { name: 'severity', weight: 1 },
          { name: 'description', weight: 0.5 },
        ],
        threshold: 0.3, // Lower = more strict matching (0.0 is perfect match)
        includeScore: true,
        minMatchCharLength: 2,
      }),
    [vulnerabilities]
  )

  // Filter vulnerabilities using Fuse.js fuzzy search AND type filter
  const filteredVulnerabilities = useMemo(() => {
    let filtered = vulnerabilities

    // Apply type filter first
    if (selectedTypes.size > 0) {
      filtered = filtered.filter((v) => v.type && selectedTypes.has(v.type))
    }

    // Then apply search query
    if (!searchQuery) return filtered

    const searchFuse = new Fuse(filtered, {
      keys: [
        { name: 'vulnerability_id', weight: 2 },
        { name: 'title', weight: 1.5 },
        { name: 'package_name', weight: 1.5 },
        { name: 'severity', weight: 1 },
        { name: 'description', weight: 0.5 },
      ],
      threshold: 0.3,
      includeScore: true,
      minMatchCharLength: 2,
    })

    const results = searchFuse.search(searchQuery)
    return results.map((result) => result.item)
  }, [searchQuery, vulnerabilities, selectedTypes])

  // Group vulnerabilities by class (os-pkgs vs lang-pkgs)
  const groupedVulnerabilities = useMemo(() => {
    const osPackages = filteredVulnerabilities.filter((v) => v.class === 'os-pkgs')
    const sourceCode = filteredVulnerabilities.filter((v) => v.class === 'lang-pkgs')
    const other = filteredVulnerabilities.filter((v) => !v.class || (v.class !== 'os-pkgs' && v.class !== 'lang-pkgs'))

    return {
      osPackages,
      sourceCode,
      other,
    }
  }, [filteredVulnerabilities])

  // Get unique vulnerability types for filtering
  const vulnerabilityTypes = useMemo(() => {
    const types = new Set<string>()
    vulnerabilities.forEach((v) => {
      if (v.type) {
        types.add(v.type)
      }
    })
    return Array.from(types).sort()
  }, [vulnerabilities])

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
    <div className="space-y-4">
      {/* Compact Scan Header */}
      <div className="flex items-center justify-between gap-4 pb-3 border-b">
        <div className="flex items-center gap-6">
          <Button variant="ghost" size="sm" asChild className="pl-0 -ml-2">
            <Link to={`/projects/${slug}/security`}>
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back
            </Link>
          </Button>
          <div className="flex items-center gap-2">
            <Shield className="h-5 w-5 text-muted-foreground" />
            <h1 className="text-lg font-semibold">Vulnerability Scan</h1>
          </div>
          <div className="flex items-center gap-3 text-xs text-muted-foreground">
            {environment && <span>Environment: {environment.name}</span>}
            <span className="text-muted-foreground/50">•</span>
            <span>Scanner: {scan.scanner_type}</span>
            {scan.scanner_version && (
              <>
                <span className="text-muted-foreground/50">•</span>
                <span>v{scan.scanner_version}</span>
              </>
            )}
            {scan.branch && (
              <>
                <span className="text-muted-foreground/50">•</span>
                <span>Branch: {scan.branch}</span>
              </>
            )}
            {scan.commit_hash && (
              <>
                <span className="text-muted-foreground/50">•</span>
                <span className="font-mono">{scan.commit_hash.substring(0, 7)}</span>
              </>
            )}
          </div>
        </div>
        <div className="flex items-center gap-3 flex-wrap justify-end">
          {/* Export button - only show if vulnerabilities exist */}
          {totalVulnerabilities > 0 && vulnerabilities.length > 0 && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm">
                  <Download className="h-4 w-4 mr-2" />
                  Export
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => exportToJSON(vulnerabilities, scanId || '0')}>
                  <FileJson className="h-4 w-4 mr-2" />
                  Export as JSON
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => exportToCSV(vulnerabilities, scanId || '0')}>
                  <FileSpreadsheet className="h-4 w-4 mr-2" />
                  Export as CSV
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}

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

      {/* Failed Scan Alert */}
      {scan.status === 'failed' && (
        <Card className="border-red-500/20 bg-red-500/5">
          <CardContent className="flex items-start gap-4 py-6">
            <div className="flex-shrink-0">
              <div className="h-12 w-12 rounded-full bg-red-500/10 flex items-center justify-center">
                <Shield className="h-6 w-6 text-red-500" />
              </div>
            </div>
            <div className="flex-1">
              <h3 className="text-lg font-semibold text-red-500 mb-2">Scan Failed</h3>
              <p className="text-sm text-muted-foreground mb-4">
                The vulnerability scan encountered an error and could not complete.
              </p>
              {scan.error_message && (
                <div className="bg-background/50 rounded-md p-4 border border-red-500/20">
                  <p className="text-sm font-mono text-foreground whitespace-pre-wrap break-words">
                    {scan.error_message}
                  </p>
                </div>
              )}
              <div className="mt-4">
                <Button variant="outline" size="sm" asChild>
                  <Link to={`/projects/${slug}/security`}>
                    <ArrowLeft className="h-4 w-4 mr-2" />
                    Back to Security
                  </Link>
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Vulnerabilities Section */}
      {scan.status === 'completed' && totalVulnerabilities > 0 && (
        <div className="space-y-4">
          <div className="flex items-center justify-between gap-4">
            <h2 className="text-xl font-semibold">Vulnerabilities</h2>
            <div className="flex items-center gap-4">
              <p className="text-sm text-muted-foreground whitespace-nowrap">
                {filteredVulnerabilities.length} of {vulnerabilities.length}{' '}
                {vulnerabilities.length === 1 ? 'vulnerability' : 'vulnerabilities'}
              </p>
            </div>
          </div>

          {/* Search and Filter Bar */}
          <div className="space-y-3">
            <div className="flex items-center gap-3">
              {/* Search Box */}
              <div className="relative flex-1 max-w-md">
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                <Input
                  type="text"
                  placeholder="Search by CVE ID, title, package, severity..."
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  className="pl-9"
                />
              </div>

              {/* Type Filter Toggle */}
              {vulnerabilityTypes.length > 0 && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setShowTypeFilters(!showTypeFilters)}
                  className="gap-2"
                >
                  <Filter className="h-4 w-4" />
                  Filter by type
                  {selectedTypes.size > 0 && (
                    <Badge variant="secondary" className="ml-1 h-5 px-1.5 text-xs">
                      {selectedTypes.size}
                    </Badge>
                  )}
                </Button>
              )}
            </div>

            {/* Collapsible Type Filter Badges */}
            {showTypeFilters && vulnerabilityTypes.length > 0 && (
              <div className="flex flex-wrap items-center gap-2 p-3 border rounded-md bg-muted/30">
                <Button
                  variant={selectedTypes.size === 0 ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => setSelectedTypes(new Set())}
                  className="h-7"
                >
                  All
                </Button>
                {vulnerabilityTypes.map((type) => {
                  const isSelected = selectedTypes.has(type)
                  const typeCount = vulnerabilities.filter((v) => v.type === type).length

                  return (
                    <Button
                      key={type}
                      variant={isSelected ? 'default' : 'outline'}
                      size="sm"
                      onClick={() => {
                        const newTypes = new Set(selectedTypes)
                        if (isSelected) {
                          newTypes.delete(type)
                        } else {
                          newTypes.add(type)
                        }
                        setSelectedTypes(newTypes)
                      }}
                      className="h-7"
                    >
                      {type}
                      <Badge
                        variant="secondary"
                        className="ml-2 h-4 px-1.5 text-xs bg-background/50"
                      >
                        {typeCount}
                      </Badge>
                    </Button>
                  )
                })}
                {selectedTypes.size > 0 && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setSelectedTypes(new Set())}
                    className="h-7 text-muted-foreground hover:text-foreground"
                  >
                    Clear filters
                  </Button>
                )}
              </div>
            )}
          </div>

          {/* Tabs for grouping vulnerabilities */}
          <Tabs defaultValue="source-code" className="w-full">
            <TabsList>
              <TabsTrigger value="all">
                All ({filteredVulnerabilities.length})
              </TabsTrigger>
              <TabsTrigger value="os-packages">
                <Package className="h-4 w-4 mr-2" />
                Container/OS ({groupedVulnerabilities.osPackages.length})
              </TabsTrigger>
              <TabsTrigger value="source-code">
                <Code className="h-4 w-4 mr-2" />
                Source Code ({groupedVulnerabilities.sourceCode.length})
              </TabsTrigger>
              {groupedVulnerabilities.other.length > 0 && (
                <TabsTrigger value="other">
                  Other ({groupedVulnerabilities.other.length})
                </TabsTrigger>
              )}
            </TabsList>

            <TabsContent value="all" className="mt-4">
              <VulnerabilityList
                vulnerabilities={filteredVulnerabilities}
                isLoading={isVulnerabilitiesLoading}
                scanId={scan.id}
                projectSlug={slug || ''}
              />
            </TabsContent>

            <TabsContent value="os-packages" className="mt-4">
              {groupedVulnerabilities.osPackages.length > 0 ? (
                <VulnerabilityList
                  vulnerabilities={groupedVulnerabilities.osPackages}
                  isLoading={isVulnerabilitiesLoading}
                  scanId={scan.id}
                  projectSlug={slug || ''}
                />
              ) : (
                <Card>
                  <CardContent className="flex flex-col items-center justify-center py-12">
                    <Package className="h-12 w-12 text-muted-foreground mb-4" />
                    <h3 className="text-lg font-medium mb-2">No container/OS vulnerabilities</h3>
                    <p className="text-muted-foreground text-center">
                      No vulnerabilities found in operating system or container packages
                    </p>
                  </CardContent>
                </Card>
              )}
            </TabsContent>

            <TabsContent value="source-code" className="mt-4">
              {groupedVulnerabilities.sourceCode.length > 0 ? (
                <VulnerabilityList
                  vulnerabilities={groupedVulnerabilities.sourceCode}
                  isLoading={isVulnerabilitiesLoading}
                  scanId={scan.id}
                  projectSlug={slug || ''}
                />
              ) : (
                <Card>
                  <CardContent className="flex flex-col items-center justify-center py-12">
                    <Code className="h-12 w-12 text-muted-foreground mb-4" />
                    <h3 className="text-lg font-medium mb-2">No source code vulnerabilities</h3>
                    <p className="text-muted-foreground text-center">
                      No vulnerabilities found in application dependencies
                    </p>
                  </CardContent>
                </Card>
              )}
            </TabsContent>

            {groupedVulnerabilities.other.length > 0 && (
              <TabsContent value="other" className="mt-4">
                <VulnerabilityList
                  vulnerabilities={groupedVulnerabilities.other}
                  isLoading={isVulnerabilitiesLoading}
                  scanId={scan.id}
                  projectSlug={slug || ''}
                />
              </TabsContent>
            )}
          </Tabs>

          {/* No results message */}
          {searchQuery && filteredVulnerabilities.length === 0 && (
            <Card>
              <CardContent className="flex flex-col items-center justify-center py-12">
                <Search className="h-12 w-12 text-muted-foreground mb-4" />
                <h3 className="text-lg font-medium mb-2">No vulnerabilities found</h3>
                <p className="text-muted-foreground text-center mb-4">
                  No vulnerabilities match your search query: "{searchQuery}"
                </p>
                <Button variant="outline" onClick={() => setSearchQuery('')}>
                  Clear search
                </Button>
              </CardContent>
            </Card>
          )}
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
