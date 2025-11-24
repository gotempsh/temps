import { ProjectResponse } from '@/api/client'
import {
  createWebhookMutation,
  listEventTypesOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertCircle, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { Alert, AlertDescription } from '@/components/ui/alert'

const formSchema = z.object({
  url: z.string().min(1, 'URL is required').url('Must be a valid URL'),
  events: z.array(z.string()).min(1, 'Select at least one event'),
  secret: z.string().optional(),
  enabled: z.boolean().optional().default(true),
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

  // Fetch available event types from API
  const {
    data: eventTypes,
    isLoading: isLoadingEventTypes,
    isError: isEventTypesError,
  } = useQuery({
    ...listEventTypesOptions(),
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

                  {isLoadingEventTypes ? (
                    <div className="flex items-center justify-center h-[300px] rounded-md border">
                      <div className="flex flex-col items-center gap-2 text-muted-foreground">
                        <Loader2 className="h-6 w-6 animate-spin" />
                        <p className="text-sm">Loading event types...</p>
                      </div>
                    </div>
                  ) : isEventTypesError ? (
                    <Alert variant="destructive">
                      <AlertCircle className="h-4 w-4" />
                      <AlertDescription>
                        Failed to load event types. Please try again later.
                      </AlertDescription>
                    </Alert>
                  ) : (
                    <ScrollArea className="h-[300px] rounded-md border p-4">
                      <div className="space-y-4">
                        {eventTypes?.map((eventType) => (
                          <FormField
                            key={eventType.event_type}
                            control={form.control}
                            name="events"
                            render={({ field }) => {
                              return (
                                <FormItem
                                  key={eventType.event_type}
                                  className="flex flex-row items-start space-x-3 space-y-0"
                                >
                                  <FormControl>
                                    <Checkbox
                                      checked={field.value?.includes(
                                        eventType.event_type
                                      )}
                                      onCheckedChange={(checked) => {
                                        return checked
                                          ? field.onChange([
                                              ...field.value,
                                              eventType.event_type,
                                            ])
                                          : field.onChange(
                                              field.value?.filter(
                                                (value) =>
                                                  value !== eventType.event_type
                                              )
                                            )
                                      }}
                                    />
                                  </FormControl>
                                  <div className="space-y-1 leading-none">
                                    <div className="flex items-center gap-2">
                                      <FormLabel className="font-medium cursor-pointer">
                                        {eventType.event_type
                                          .split('.')
                                          .map(
                                            (word) =>
                                              word.charAt(0).toUpperCase() +
                                              word.slice(1)
                                          )
                                          .join(' ')}
                                      </FormLabel>
                                      <span className="text-xs text-muted-foreground bg-muted px-2 py-0.5 rounded">
                                        {eventType.category}
                                      </span>
                                    </div>
                                    <FormDescription className="text-xs">
                                      {eventType.description}
                                    </FormDescription>
                                  </div>
                                </FormItem>
                              )
                            }}
                          />
                        ))}
                      </div>
                    </ScrollArea>
                  )}
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
