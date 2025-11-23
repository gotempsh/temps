import { ProjectResponse } from '@/api/client'
import { createWebhookMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import { ScrollArea } from '@/components/ui/scroll-area'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const AVAILABLE_EVENTS = [
  { id: 'deployment.started', label: 'Deployment Started', description: 'Triggered when a deployment begins' },
  { id: 'deployment.succeeded', label: 'Deployment Succeeded', description: 'Triggered when a deployment completes successfully' },
  { id: 'deployment.failed', label: 'Deployment Failed', description: 'Triggered when a deployment fails' },
  { id: 'error.created', label: 'Error Created', description: 'Triggered when a new error is detected' },
  { id: 'monitor.down', label: 'Monitor Down', description: 'Triggered when a monitor detects downtime' },
  { id: 'monitor.up', label: 'Monitor Up', description: 'Triggered when a monitor recovers' },
  { id: 'domain.verified', label: 'Domain Verified', description: 'Triggered when a domain is successfully verified' },
  { id: 'domain.failed', label: 'Domain Verification Failed', description: 'Triggered when domain verification fails' },
]

const formSchema = z.object({
  url: z.string().url('Must be a valid URL').min(1, 'URL is required'),
  events: z.array(z.string()).min(1, 'Select at least one event'),
  secret: z.string().optional(),
  enabled: z.boolean().default(true),
})

type FormValues = z.infer<typeof formSchema>

interface CreateWebhookDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  project: ProjectResponse
  onSuccess: () => void
}

export function CreateWebhookDialog({
  open,
  onOpenChange,
  project,
  onSuccess,
}: CreateWebhookDialogProps) {
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
      form.reset()
      onSuccess()
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

  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen && !createWebhook.isPending) {
      form.reset()
    }
    onOpenChange(newOpen)
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Create Webhook</DialogTitle>
          <DialogDescription>
            Configure a webhook endpoint to receive real-time notifications about
            events in your project
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
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
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="events"
              render={() => (
                <FormItem>
                  <div className="mb-4">
                    <FormLabel>Events</FormLabel>
                    <FormDescription>
                      Select which events should trigger this webhook
                    </FormDescription>
                  </div>
                  <ScrollArea className="h-[300px] rounded-md border p-4">
                    <div className="space-y-4">
                      {AVAILABLE_EVENTS.map((event) => (
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
                                    checked={field.value?.includes(event.id)}
                                    onCheckedChange={(checked) => {
                                      return checked
                                        ? field.onChange([...field.value, event.id])
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
                  </ScrollArea>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="secret"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Secret (Optional)</FormLabel>
                  <FormControl>
                    <Input
                      type="password"
                      placeholder="Enter a secret for HMAC signature verification"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    Used to verify webhook authenticity via HMAC signatures in the
                    X-Webhook-Signature header
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

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => handleOpenChange(false)}
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
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
