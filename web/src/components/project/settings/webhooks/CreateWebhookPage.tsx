import { ProjectResponse } from '@/api/client'
import { createWebhookMutation } from '@/api/client/@tanstack/react-query.gen'
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
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { ArrowLeft, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

const AVAILABLE_EVENTS = [
  {
    category: 'Deployments',
    events: [
      {
        id: 'deployment.started',
        label: 'Deployment Started',
        description: 'Triggered when a deployment begins',
      },
      {
        id: 'deployment.succeeded',
        label: 'Deployment Succeeded',
        description: 'Triggered when a deployment completes successfully',
      },
      {
        id: 'deployment.failed',
        label: 'Deployment Failed',
        description: 'Triggered when a deployment fails',
      },
    ],
  },
  {
    category: 'Error Tracking',
    events: [
      {
        id: 'error.created',
        label: 'Error Created',
        description: 'Triggered when a new error is detected',
      },
    ],
  },
  {
    category: 'Monitoring',
    events: [
      {
        id: 'monitor.down',
        label: 'Monitor Down',
        description: 'Triggered when a monitor detects downtime',
      },
      {
        id: 'monitor.up',
        label: 'Monitor Up',
        description: 'Triggered when a monitor recovers',
      },
    ],
  },
  {
    category: 'Domains',
    events: [
      {
        id: 'domain.verified',
        label: 'Domain Verified',
        description: 'Triggered when a domain is successfully verified',
      },
      {
        id: 'domain.failed',
        label: 'Domain Verification Failed',
        description: 'Triggered when domain verification fails',
      },
    ],
  },
]

const formSchema = z.object({
  url: z.string().url('Must be a valid URL').min(1, 'URL is required'),
  events: z.array(z.string()).min(1, 'Select at least one event'),
  secret: z.string().optional(),
  enabled: z.boolean().default(true),
})

type FormValues = z.infer<typeof formSchema>

interface CreateWebhookPageProps {
  project: ProjectResponse
}

export function CreateWebhookPage({ project }: CreateWebhookPageProps) {
  const navigate = useNavigate()

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
              <FormField
                control={form.control}
                name="events"
                render={() => (
                  <FormItem>
                    <div className="space-y-6">
                      {AVAILABLE_EVENTS.map((category, categoryIndex) => (
                        <div key={category.category}>
                          {categoryIndex > 0 && <Separator className="my-4" />}
                          <div className="space-y-4">
                            <h4 className="text-sm font-semibold">
                              {category.category}
                            </h4>
                            <div className="space-y-4">
                              {category.events.map((event) => (
                                <FormField
                                  key={event.id}
                                  control={form.control}
                                  name="events"
                                  render={({ field }) => {
                                    return (
                                      <FormItem
                                        key={event.id}
                                        className="flex flex-row items-start space-x-3 space-y-0"
                                      >
                                        <FormControl>
                                          <Checkbox
                                            checked={field.value?.includes(
                                              event.id
                                            )}
                                            onCheckedChange={(checked) => {
                                              return checked
                                                ? field.onChange([
                                                    ...field.value,
                                                    event.id,
                                                  ])
                                                : field.onChange(
                                                    field.value?.filter(
                                                      (value) => value !== event.id
                                                    )
                                                  )
                                            }}
                                          />
                                        </FormControl>
                                        <div className="space-y-1 leading-none">
                                          <FormLabel className="font-medium cursor-pointer">
                                            {event.label}
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
                      ))}
                    </div>
                    <FormMessage className="mt-4" />
                  </FormItem>
                )}
              />
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
