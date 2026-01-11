import { ProjectResponse } from '@/api/client'
import {
  createWebhookMutation,
  listEventTypesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'
import { Alert, AlertDescription } from '@/components/ui/alert'

const formSchema = z.object({
  url: z.string().min(1, 'URL is required').url('Must be a valid URL'),
  events: z.array(z.string()).min(1, 'Select at least one event'),
  secret: z.string().optional(),
  enabled: z.boolean(),
})

type FormValues = z.infer<typeof formSchema>

interface CreateWebhookPageProps {
  project: ProjectResponse
}

export function CreateWebhookPage({ project }: CreateWebhookPageProps) {
  const navigate = useNavigate()

  // Fetch available event types from API
  const {
    data: eventTypes,
    isLoading: isLoadingEventTypes,
    isError: isEventTypesError,
  } = useQuery({
    ...listEventTypesOptions(),
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      url: '',
      events: [],
      secret: '',
      enabled: true,
    },
  })

  const createWebhook = useMutation({
    ...createWebhookMutation(),
    onSuccess: () => {
      toast.success('Webhook created successfully')
      navigate(`/projects/${project.slug}/settings/webhooks`)
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to create webhook')
    },
  })

  const onSubmit = (values: FormValues) => {
    createWebhook.mutate({
      path: {
        project_id: project.id,
      },
      body: {
        url: values.url,
        events: values.events,
        secret: values.secret || null,
        enabled: values.enabled,
      },
    })
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => navigate(`/projects/${project.slug}/settings/webhooks`)}
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Create Webhook</h2>
          <p className="text-muted-foreground">
            Configure a webhook endpoint to receive real-time notifications
          </p>
        </div>
      </div>

      <Form {...form}>
        <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Endpoint Configuration</CardTitle>
              <CardDescription>
                Specify the URL where webhook events will be delivered
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <FormField
                control={form.control}
                name="url"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Webhook URL</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="https://example.com/webhooks"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      The endpoint that will receive webhook events via HTTP POST
                      requests
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="secret"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Signing Secret (Optional)</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="Enter a secret for HMAC signature verification"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      Used to verify webhook authenticity via HMAC signatures in
                      the X-Webhook-Signature header. If provided, all webhook
                      requests will include a signature you can verify.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="enabled"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">Enable webhook</FormLabel>
                      <FormDescription>
                        Start receiving events immediately after creation
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Event Subscriptions</CardTitle>
              <CardDescription>
                Choose which events should trigger this webhook
              </CardDescription>
            </CardHeader>
            <CardContent>
              {isLoadingEventTypes ? (
                <div className="space-y-4">
                  <Skeleton className="h-4 w-32" />
                  <Skeleton className="h-10 w-full" />
                  <Skeleton className="h-10 w-full" />
                  <Skeleton className="h-10 w-full" />
                </div>
              ) : isEventTypesError ? (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    Failed to load event types. Please try again later.
                  </AlertDescription>
                </Alert>
              ) : (
                <FormField
                  control={form.control}
                  name="events"
                  render={() => {
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
                      <FormItem>
                        <div className="space-y-6">
                          {Object.entries(eventsByCategory).map(
                            ([category, events], categoryIndex) => (
                              <div key={category}>
                                {categoryIndex > 0 && (
                                  <Separator className="my-4" />
                                )}
                                <div className="space-y-4">
                                  <h4 className="text-sm font-semibold">
                                    {category}
                                  </h4>
                                  <div className="space-y-4">
                                    {events.map((event) => (
                                      <FormField
                                        key={event.event_type}
                                        control={form.control}
                                        name="events"
                                        render={({ field }) => {
                                          return (
                                            <FormItem
                                              key={event.event_type}
                                              className="flex flex-row items-start space-x-3 space-y-0"
                                            >
                                              <FormControl>
                                                <Checkbox
                                                  checked={field.value?.includes(
                                                    event.event_type
                                                  )}
                                                  onCheckedChange={(checked) => {
                                                    return checked
                                                      ? field.onChange([
                                                          ...field.value,
                                                          event.event_type,
                                                        ])
                                                      : field.onChange(
                                                          field.value?.filter(
                                                            (value) =>
                                                              value !==
                                                              event.event_type
                                                          )
                                                        )
                                                  }}
                                                />
                                              </FormControl>
                                              <div className="space-y-1 leading-none">
                                                <FormLabel className="font-medium cursor-pointer">
                                                  {event.event_type
                                                    .split('.')
                                                    .map(
                                                      (word) =>
                                                        word
                                                          .charAt(0)
                                                          .toUpperCase() +
                                                        word.slice(1)
                                                    )
                                                    .join(' ')}
                                                </FormLabel>
                                                <FormDescription className="text-xs">
                                                  {event.description}
                                                </FormDescription>
                                              </div>
                                            </FormItem>
                                          )
                                        }}
                                      />
                                    ))}
                                  </div>
                                </div>
                              </div>
                            )
                          )}
                        </div>
                        <FormMessage className="mt-4" />
                      </FormItem>
                    )
                  }}
                />
              )}
            </CardContent>
          </Card>

          <div className="flex justify-end gap-3">
            <Button
              type="button"
              variant="outline"
              onClick={() =>
                navigate(`/projects/${project.slug}/settings/webhooks`)
              }
              disabled={createWebhook.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={createWebhook.isPending}>
              {createWebhook.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Create Webhook
            </Button>
          </div>
        </form>
      </Form>
    </div>
  )
}
