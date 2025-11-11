import { ContainerDetailResponse } from '@/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'

interface ContainerConfigurationProps {
  container: ContainerDetailResponse
}

export function ContainerConfiguration({
  container,
}: ContainerConfigurationProps) {
  return (
    <div className="space-y-6">
      {/* Basic Info */}
      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Basic Information</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <label className="text-sm font-medium text-muted-foreground">
              Container ID
            </label>
            <p className="text-sm mt-1 font-mono bg-muted p-2 rounded break-all">
              {container.container_id}
            </p>
          </div>
          <div>
            <label className="text-sm font-medium text-muted-foreground">
              Image
            </label>
            <p className="text-sm mt-1 font-mono bg-muted p-2 rounded break-all">
              {container.image_name}
            </p>
          </div>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="text-sm font-medium text-muted-foreground">
                Status
              </label>
              <div className="mt-1">
                <Badge
                  variant={
                    container.status === 'running' ? 'default' : 'secondary'
                  }
                >
                  {container.status}
                </Badge>
              </div>
            </div>
            <div>
              <label className="text-sm font-medium text-muted-foreground">
                Uptime
              </label>
              <p className="text-sm mt-1">
                {formatUptimeFromTimestamp(container.created_at)}
              </p>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Ports */}
      {(container.container_port || container.host_port) && (
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Ports</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              <div className="p-2 rounded bg-muted">
                <div className="text-sm font-medium text-muted-foreground">
                  Container Port
                </div>
                <div className="text-sm font-mono mt-1">
                  {container.container_port}
                </div>
              </div>
              {container.host_port && (
                <div className="p-2 rounded bg-muted">
                  <div className="text-sm font-medium text-muted-foreground">
                    Host Port
                  </div>
                  <div className="text-sm font-mono mt-1">
                    {container.host_port}
                  </div>
                </div>
              )}
            </div>
          </CardContent>
        </Card>
      )}

      {/* Environment Variables */}
      {container.environment_variables &&
        (Array.isArray(container.environment_variables)
          ? container.environment_variables.length > 0
          : Object.keys(container.environment_variables).length > 0) && (
          <Card>
            <CardHeader>
              <CardTitle className="text-lg">Environment Variables</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-2 max-h-96 overflow-y-auto">
                {Array.isArray(container.environment_variables)
                  ? container.environment_variables.map(
                      (envVar: any, idx: number) => {
                        let key = ''
                        let value = ''

                        if (typeof envVar === 'string') {
                          // Handle "KEY=VALUE" format
                          const [k, v] = envVar.split('=')
                          key = k
                          value = v
                        } else if (
                          typeof envVar === 'object' &&
                          envVar !== null
                        ) {
                          // Handle object format - try common property names
                          if ('name' in envVar && 'value' in envVar) {
                            key = String((envVar as any).name)
                            value = String((envVar as any).value)
                          } else if ('key' in envVar && 'value' in envVar) {
                            key = String((envVar as any).key)
                            value = String((envVar as any).value)
                          } else {
                            // Fallback: use first two properties
                            const entries = Object.entries(envVar)
                            if (entries.length >= 2) {
                              key = String(entries[0][1])
                              value = String(entries[1][1])
                            } else if (entries.length === 1) {
                              key = String(entries[0][0])
                              value = String(entries[0][1])
                            }
                          }
                        }

                        if (!key) return null

                        return (
                          <div
                            key={idx}
                            className="p-2 rounded bg-muted text-sm font-mono"
                          >
                            <div className="font-medium text-foreground">
                              {key}
                            </div>
                            <div className="text-muted-foreground break-all">
                              {value || 'N/A'}
                            </div>
                          </div>
                        )
                      }
                    )
                  : Object.entries(container.environment_variables).map(
                      ([key, value]) => (
                        <div
                          key={key}
                          className="p-2 rounded bg-muted text-sm font-mono"
                        >
                          <div className="font-medium text-foreground">
                            {key}
                          </div>
                          <div className="text-muted-foreground break-all">
                            {String(value)}
                          </div>
                        </div>
                      )
                    )}
              </div>
            </CardContent>
          </Card>
        )}
    </div>
  )
}

function formatUptimeFromTimestamp(createdAt?: string): string {
  if (!createdAt) return 'N/A'
  const elapsedMs = Date.now() - new Date(createdAt).getTime()
  const elapsedSeconds = Math.floor(elapsedMs / 1000)
  return formatUptime(elapsedSeconds)
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${Math.floor(seconds)}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}
