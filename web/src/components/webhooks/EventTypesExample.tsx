/**
 * Example component demonstrating how to fetch and display webhook event types
 * from the API using the generated client.
 *
 * This component can be imported and used anywhere you need to display available
 * webhook event types.
 */

import { listEventTypesOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { Loader2, AlertCircle } from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'

/**
 * EventTypesExample Component
 *
 * Demonstrates fetching webhook event types from the API endpoint:
 * GET /webhook-event-types
 *
 * The API client is auto-generated from OpenAPI spec and provides:
 * - listEventTypes() - SDK function for direct API calls
 * - listEventTypesOptions() - React Query hook options
 * - listEventTypesQueryKey() - Query key for cache management
 */
export function EventTypesExample() {
  // Fetch event types using React Query
  const { data: eventTypes, isLoading, isError } = useQuery({
    ...listEventTypesOptions(),
  })

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Webhook Event Types</CardTitle>
          <CardDescription>
            Loading available webhook event types...
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
          </div>
        </CardContent>
      </Card>
    )
  }

  if (isError) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Webhook Event Types</CardTitle>
          <CardDescription>Failed to load event types</CardDescription>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Failed to load webhook event types. Please try again later.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  // Group events by category
  const eventsByCategory =
    eventTypes?.reduce(
      (acc, eventType) => {
        const category = eventType.category
        if (!acc[category]) {
          acc[category] = []
        }
        acc[category].push(eventType)
        return acc
      },
      {} as Record<string, typeof eventTypes>
    ) || {}

  return (
    <Card>
      <CardHeader>
        <CardTitle>Webhook Event Types</CardTitle>
        <CardDescription>
          All available webhook event types grouped by category
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="space-y-6">
          {Object.entries(eventsByCategory).map(([category, events]) => (
            <div key={category} className="space-y-3">
              <h3 className="text-lg font-semibold">{category}</h3>
              <div className="space-y-2">
                {events.map((event) => (
                  <div
                    key={event.event_type}
                    className="flex items-start gap-3 rounded-lg border p-3"
                  >
                    <div className="flex-1 space-y-1">
                      <div className="flex items-center gap-2">
                        <code className="text-sm font-mono bg-muted px-2 py-1 rounded">
                          {event.event_type}
                        </code>
                        <Badge variant="secondary">{event.category}</Badge>
                      </div>
                      <p className="text-sm text-muted-foreground">
                        {event.description}
                      </p>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div className="mt-6 pt-6 border-t">
          <h4 className="text-sm font-semibold mb-2">API Usage Example:</h4>
          <pre className="text-xs bg-muted p-3 rounded overflow-x-auto">
            {`import { listEventTypesOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

const { data, isLoading, isError } = useQuery({
  ...listEventTypesOptions(),
})

// data: Array<EventTypeResponse>
// EventTypeResponse: {
//   event_type: string
//   category: string
//   description: string
// }`}
          </pre>
        </div>
      </CardContent>
    </Card>
  )
}
