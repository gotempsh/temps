import { client } from '@/api/client/client.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useQuery } from '@tanstack/react-query'
import { Download, FileText, ScrollText } from 'lucide-react'
import { useState } from 'react'

// These types mirror the backend DeploymentContainerLog* response DTOs. Once
// the OpenAPI SDK is regenerated (bun run openapi-ts) the generated
// types/wrappers can replace these and the raw `client.get` calls below.
interface DeploymentContainerLog {
  id: number
  deployment_id: number
  container_id: string
  container_name: string
  service_name: string | null
  node_id: number | null
  size_bytes: number
  truncated: boolean
  captured_at: number
}

interface DeploymentContainerLogsListResponse {
  logs: DeploymentContainerLog[]
}

interface DeploymentContainerLogContentResponse {
  id: number
  container_name: string
  service_name: string | null
  size_bytes: number
  truncated: boolean
  captured_at: number
  content: string
}

const bearer = [{ scheme: 'bearer' as const, type: 'http' as const }]

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

async function fetchCapturedLogs(
  projectId: number,
  deploymentId: number
): Promise<DeploymentContainerLogsListResponse> {
  const response = await client.get({
    url: `/projects/${projectId}/deployments/${deploymentId}/container-logs`,
    security: bearer,
  })
  return response.data as DeploymentContainerLogsListResponse
}

async function fetchCapturedLogContent(
  projectId: number,
  deploymentId: number,
  logId: number
): Promise<DeploymentContainerLogContentResponse> {
  const response = await client.get({
    url: `/projects/${projectId}/deployments/${deploymentId}/container-logs/${logId}`,
    security: bearer,
  })
  return response.data as DeploymentContainerLogContentResponse
}

interface DeploymentContainerLogsProps {
  projectId: number
  deploymentId: number
}

/**
 * Shows the logs that were captured for this deployment's containers just
 * before they were torn down. Lets users read the output of a container that
 * no longer exists (e.g. "web-2" from a previous deployment).
 */
export function DeploymentContainerLogs({
  projectId,
  deploymentId,
}: DeploymentContainerLogsProps) {
  const [selectedLogId, setSelectedLogId] = useState<number | null>(null)

  const {
    data: list,
    isPending,
    isError,
  } = useQuery({
    queryKey: ['deployment-container-logs', projectId, deploymentId],
    queryFn: () => fetchCapturedLogs(projectId, deploymentId),
    enabled: projectId > 0 && deploymentId > 0,
  })

  const { data: content, isPending: isContentPending } = useQuery({
    queryKey: [
      'deployment-container-log-content',
      projectId,
      deploymentId,
      selectedLogId,
    ],
    queryFn: () =>
      fetchCapturedLogContent(projectId, deploymentId, selectedLogId as number),
    enabled: selectedLogId != null,
  })

  // The capture feature is best-effort and only runs on teardown, so most
  // deployments legitimately have nothing captured. Render nothing rather than
  // an empty card to avoid clutter on the deployment detail page.
  if (isError) return null
  if (isPending) {
    return (
      <Card>
        <CardContent className="p-6 space-y-3">
          <Skeleton className="h-5 w-48" />
          <Skeleton className="h-10 w-full" />
        </CardContent>
      </Card>
    )
  }
  if (!list || list.logs.length === 0) return null

  return (
    <Card>
      <CardContent className="p-6">
        <div className="flex items-center gap-2 mb-4">
          <ScrollText className="h-4 w-4 text-muted-foreground" />
          <h3 className="text-sm font-semibold">Captured container logs</h3>
          <Badge variant="secondary" className="ml-1">
            {list.logs.length}
          </Badge>
        </div>
        <p className="text-xs text-muted-foreground mb-4">
          Logs captured from this deployment&apos;s containers before they were
          torn down. Available even after the containers no longer exist.
        </p>

        <div className="flex flex-col gap-2">
          {list.logs.map((log) => {
            const isSelected = log.id === selectedLogId
            return (
              <div
                key={log.id}
                className="rounded-md border border-border/60 overflow-hidden"
              >
                <button
                  type="button"
                  onClick={() =>
                    setSelectedLogId(isSelected ? null : log.id)
                  }
                  className="w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-muted/50 transition-colors"
                >
                  <FileText className="h-4 w-4 text-muted-foreground shrink-0" />
                  <span className="text-sm font-mono truncate">
                    {log.container_name}
                  </span>
                  {log.service_name && (
                    <Badge variant="outline" className="shrink-0">
                      {log.service_name}
                    </Badge>
                  )}
                  {log.truncated && (
                    <Badge variant="outline" className="shrink-0">
                      truncated
                    </Badge>
                  )}
                  <span className="ml-auto text-xs text-muted-foreground shrink-0">
                    {formatBytes(log.size_bytes)}
                  </span>
                  <span className="text-xs text-muted-foreground shrink-0 hidden sm:inline">
                    <TimeAgo date={new Date(log.captured_at)} />
                  </span>
                </button>

                {isSelected && (
                  <div className="border-t border-border/60 bg-muted/30">
                    {isContentPending ? (
                      <div className="p-3 space-y-2">
                        <Skeleton className="h-3 w-full" />
                        <Skeleton className="h-3 w-5/6" />
                        <Skeleton className="h-3 w-4/6" />
                      </div>
                    ) : (
                      <div>
                        <div className="flex items-center justify-end px-3 py-2 border-b border-border/40">
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => {
                              if (!content) return
                              const blob = new Blob([content.content], {
                                type: 'text/plain',
                              })
                              const url = URL.createObjectURL(blob)
                              const a = document.createElement('a')
                              a.href = url
                              a.download = `${log.container_name}.log`
                              a.click()
                              URL.revokeObjectURL(url)
                            }}
                          >
                            <Download className="h-3.5 w-3.5 mr-1.5" />
                            Download
                          </Button>
                        </div>
                        <pre className="max-h-96 overflow-auto p-3 text-xs font-mono whitespace-pre-wrap break-all">
                          {content?.content || '(empty)'}
                        </pre>
                      </div>
                    )}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </CardContent>
    </Card>
  )
}
