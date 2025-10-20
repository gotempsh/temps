import { ProjectResponse } from '@/api/client'
import { TraceViewer } from './trace-viewer'
import { useQuery } from '@tanstack/react-query'
import { getTraceDetailsOptions } from '@/api/client/@tanstack/react-query.gen'

const exampleTrace = {
  id: '4737e2c',
  name: 'frontend: HTTP GET /dispatch',
  startTimeUnixNano: 1545000000000000000, // Example timestamp
  endTimeUnixNano: 1545000000700680000, // 700.68ms later
  services: 6,
  depth: 5,
  totalSpans: 50,
  spans: [
    {
      id: '1',
      serviceName: 'frontend',
      name: 'HTTP GET /dispatch',
      operation: 'HTTP GET /dispatch',
      startTimeUnixNano: 1545000000000000000,
      endTimeUnixNano: 1545000000700680000,
      children: [
        {
          id: '2',
          serviceName: 'frontend',
          name: 'HTTP GET: /customer',
          operation: 'HTTP GET: /customer',
          startTimeUnixNano: 1545000000000000000,
          endTimeUnixNano: 1545000000310740000,
          children: [
            {
              id: '3',
              serviceName: 'customer',
              name: 'HTTP GET /customer',
              operation: 'HTTP GET /customer',
              startTimeUnixNano: 1545000000000000000,
              endTimeUnixNano: 1545000000310400000,
              children: [
                {
                  id: '4',
                  serviceName: 'mysql',
                  name: 'SQL SELECT',
                  operation: 'SQL SELECT',
                  startTimeUnixNano: 1545000000000000000,
                  endTimeUnixNano: 1545000000310310000,
                },
              ],
            },
          ],
        },
        {
          id: '5',
          serviceName: 'redis',
          name: 'GetDriver',
          operation: 'GetDriver',
          startTimeUnixNano: 1545000000310740000,
          endTimeUnixNano: 1545000000345140000,
          error: true,
        },
      ],
    },
  ],
}

export default function Trace({
  databaseDate,
  traceId,
  project,
}: {
  databaseDate: string
  traceId: string
  project: ProjectResponse
}) {
  const {
    data: trace,
    isLoading,
    error,
  } = useQuery({
    ...getTraceDetailsOptions({
      path: {
        project_id: project.id, // Convert to string to fix type error
        trace_id: traceId,
      },
      query: {
        database_date: databaseDate,
      },
    }),
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-8">
        <div className="space-y-4 w-full max-w-2xl">
          <div className="space-y-2">
            <div className="h-4 w-48 bg-muted animate-pulse rounded" />
            <div className="h-4 w-full bg-muted animate-pulse rounded" />
          </div>
          <div className="space-y-2">
            <div className="h-24 w-full bg-muted animate-pulse rounded" />
            <div className="h-24 w-full bg-muted animate-pulse rounded" />
          </div>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="flex items-center justify-center p-8">
        <div className="text-center space-y-2">
          <p className="text-sm text-muted-foreground">
            Failed to load trace details
          </p>
          <p className="text-xs text-destructive">{error.message}</p>
        </div>
      </div>
    )
  }

  if (!trace) {
    return (
      <div className="flex items-center justify-center p-8">
        <p className="text-sm text-muted-foreground">No trace data found</p>
      </div>
    )
  }

  return <TraceViewer trace={trace} />
}
