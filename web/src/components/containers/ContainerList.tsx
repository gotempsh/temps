import { ContainerInfoResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cn } from '@/lib/utils'
import { useState, useEffect } from 'react'

interface ContainerListProps {
  containers: ContainerInfoResponse[]
  selectedId: string | null
  onSelect: (id: string) => void
}

export function ContainerList({
  containers,
  selectedId,
  onSelect,
}: ContainerListProps) {
  const [statusFilter, setStatusFilter] = useState<
    'all' | 'running' | 'stopped'
  >('all')

  const filtered = containers.filter((c) => {
    if (statusFilter === 'all') return true
    return statusFilter === 'running'
      ? c.status === 'running'
      : c.status !== 'running'
  })

  return (
    <div className="w-80 border-r bg-muted/30 flex flex-col">
      {/* Header */}
      <div className="p-4 border-b">
        <Tabs
          value={statusFilter}
          onValueChange={(value) =>
            setStatusFilter(value as 'all' | 'running' | 'stopped')
          }
        >
          <TabsList className="w-full">
            <TabsTrigger value="all" className="flex-1">
              All
            </TabsTrigger>
            <TabsTrigger value="running" className="flex-1">
              Running
            </TabsTrigger>
            <TabsTrigger value="stopped" className="flex-1">
              Stopped
            </TabsTrigger>
          </TabsList>
        </Tabs>
      </div>

      {/* Container List */}
      <ScrollArea className="flex-1">
        <div className="p-2 space-y-2">
          {filtered.map((container) => (
            <ContainerCard
              key={container.container_id}
              container={container}
              isSelected={container.container_id === selectedId}
              onSelect={onSelect}
            />
          ))}
        </div>
      </ScrollArea>
    </div>
  )
}

interface ContainerCardProps {
  container: ContainerInfoResponse
  isSelected: boolean
  onSelect: (id: string) => void
}

function UptimeDisplay({ createdAt }: { createdAt: string }) {
  const [uptime, setUptime] = useState(0)

  useEffect(() => {
    const updateUptime = () => {
      const elapsedMs = Date.now() - new Date(createdAt).getTime()
      const elapsedMinutes = Math.floor(elapsedMs / 1000 / 60)
      setUptime(elapsedMinutes)
    }

    updateUptime()
    const interval = setInterval(updateUptime, 60000) // Update every minute

    return () => clearInterval(interval)
  }, [createdAt])

  return (
    <p className="text-xs text-muted-foreground shrink-0">{uptime}m uptime</p>
  )
}

function ContainerCard({
  container,
  isSelected,
  onSelect,
}: ContainerCardProps) {
  const statusColor =
    {
      running: 'bg-green-500',
      stopped: 'bg-gray-400',
      error: 'bg-red-500',
    }[container.status] || 'bg-gray-400'

  return (
    <Card
      className={cn(
        'p-3 cursor-pointer transition-all hover:shadow-md active:shadow-sm',
        isSelected
          ? 'ring-2 ring-primary bg-primary/5 hover:bg-primary/10'
          : 'hover:bg-accent/50'
      )}
      onClick={() => onSelect(container.container_id)}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          onSelect(container.container_id)
        }
      }}
    >
      <div className="space-y-2">
        <div className="flex items-start justify-between">
          <div className="flex items-start gap-2 flex-1">
            <div
              className={cn(
                'w-2 h-2 rounded-full mt-1.5 shrink-0 animate-pulse',
                statusColor
              )}
            />
            <div className="flex-1 min-w-0">
              <p className="font-semibold text-sm break-words leading-tight">
                {container.container_name}
              </p>
              <p className="text-xs text-muted-foreground break-words leading-tight">
                {container.image_name}
              </p>
            </div>
          </div>
        </div>
        <div className="flex items-center justify-between gap-2">
          <Badge
            variant={container.status === 'running' ? 'default' : 'secondary'}
            className="text-xs shrink-0"
          >
            {container.status}
          </Badge>
          <UptimeDisplay createdAt={container.created_at} />
        </div>
      </div>
    </Card>
  )
}
