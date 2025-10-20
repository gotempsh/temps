import { ProjectResponse } from '@/api/client'
import Trace from '@/components/opentelemetry/Trace'
import { usePageTitle } from '@/hooks/usePageTitle'

export function TraceDetails({
  databaseDate,
  traceId,
  project,
}: {
  databaseDate: string
  traceId: string
  project: ProjectResponse
}) {
  usePageTitle(`${project.name} - Trace ${traceId.slice(0, 8)}`)
  return (
    <Trace databaseDate={databaseDate} traceId={traceId} project={project} />
  )
}
