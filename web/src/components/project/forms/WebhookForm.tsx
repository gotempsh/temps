import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentDomainsOptions,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
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
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import * as z from 'zod'

const webhookEvents = [
  { id: 'payment_intent.succeeded', label: 'Payment Intent Succeeded' },
  { id: 'payment_intent.payment_failed', label: 'Payment Intent Failed' },
  { id: 'payment_intent.created', label: 'Payment Intent Created' },
  { id: 'customer.subscription.created', label: 'Subscription Created' },
  { id: 'customer.subscription.updated', label: 'Subscription Updated' },
  { id: 'customer.subscription.deleted', label: 'Subscription Deleted' },
  {
    id: 'customer.subscription.trial_will_end',
    label: 'Subscription Trial Ending',
  },
  { id: 'customer.created', label: 'Customer Created' },
  { id: 'customer.updated', label: 'Customer Updated' },
  { id: 'customer.deleted', label: 'Customer Deleted' },
  { id: 'invoice.paid', label: 'Invoice Paid' },
  { id: 'invoice.payment_failed', label: 'Invoice Payment Failed' },
  { id: 'invoice.upcoming', label: 'Invoice Upcoming' },
  { id: 'checkout.session.completed', label: 'Checkout Completed' },
  { id: 'checkout.session.expired', label: 'Checkout Expired' },
] as const

const webhookFormSchema = z
  .object({
    urlType: z.enum(['environment', 'custom']),
    environment: z.string().optional(),
    domain: z.string().optional(),
    path: z.string().optional(),
    customUrl: z.string().url().optional(),
    events: z.array(z.string()).min(1, 'Select at least one event'),
  })
  .refine(
    (data) => {
      if (data.urlType === 'environment') {
        return !!data.environment && !!data.domain && !!data.path
      } else {
        return !!data.customUrl
      }
    },
    {
      message: 'Please provide all required fields for the selected URL type',
    }
  )

type WebhookFormValues = z.infer<typeof webhookFormSchema>

interface WebhookFormProps {
  project: ProjectResponse
  initialData?: Partial<WebhookFormValues>
  onSubmit: (data: WebhookFormValues) => void
  onCancel: () => void
  disabled?: boolean
}

export function WebhookForm({
  project,
  initialData,
  onSubmit,
  onCancel,
  disabled,
}: WebhookFormProps) {
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const form = useForm<WebhookFormValues>({
    resolver: zodResolver(webhookFormSchema),
    defaultValues: initialData || {
      urlType: 'environment',
      events: [],
    },
  })

  const selectedEnvironment = form.watch('environment')
  const { data: domains, isLoading: isLoadingDomains } = useQuery({
    ...getEnvironmentDomainsOptions({
      path: {
        project_id: project.id,
        env_id_or_slug: selectedEnvironment || '',
      },
    }),
    enabled: !!selectedEnvironment,
  })

  const urlType = form.watch('urlType')
  const selectedDomain = form.watch('domain')

  return (
    <Form {...form}>
      <div className="flex flex-col h-[calc(100vh-16rem)]">
        <div className="flex-1 overflow-y-auto pr-6 -mr-6">
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
            <FormField
              control={form.control}
              name="urlType"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>URL Configuration</FormLabel>
                  <FormControl>
                    <RadioGroup
                      onValueChange={field.onChange}
                      defaultValue={field.value}
                      className="flex flex-col space-y-1"
                    >
                      <FormItem className="flex items-center space-x-3 space-y-0">
                        <FormControl>
                          <RadioGroupItem value="environment" />
                        </FormControl>
                        <FormLabel className="font-normal">
                          Use Environment + Domain
                        </FormLabel>
                      </FormItem>
                      <FormItem className="flex items-center space-x-3 space-y-0">
                        <FormControl>
                          <RadioGroupItem value="custom" />
                        </FormControl>
                        <FormLabel className="font-normal">
                          Custom URL
                        </FormLabel>
                      </FormItem>
                    </RadioGroup>
                  </FormControl>
                </FormItem>
              )}
            />

            {urlType === 'environment' ? (
              <>
                <FormField
                  control={form.control}
                  name="environment"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Environment</FormLabel>
                      <Select
                        onValueChange={field.onChange}
                        defaultValue={field.value}
                        disabled={isLoadingEnvironments}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue placeholder="Select environment" />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {environments?.map((env) => (
                            <SelectItem key={env.id} value={env.id.toString()}>
                              {env.name}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="domain"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Domain</FormLabel>
                      <Select
                        onValueChange={field.onChange}
                        defaultValue={field.value}
                        disabled={isLoadingDomains || !selectedEnvironment}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue placeholder="Select domain" />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {domains?.map((domain) => (
                            <SelectItem key={domain.id} value={domain.domain}>
                              {domain.domain}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="path"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Path</FormLabel>
                      <FormControl>
                        <Input placeholder="/webhooks/stripe" {...field} />
                      </FormControl>
                      <FormDescription>
                        {selectedDomain &&
                          `https://${selectedDomain}${field.value || ''}`}
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </>
            ) : (
              <FormField
                control={form.control}
                name="customUrl"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Custom URL</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="https://your-custom-domain.com/webhooks/stripe"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      Enter a complete URL including the protocol (https://)
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            <FormField
              control={form.control}
              name="events"
              render={() => (
                <FormItem>
                  <div className="mb-4">
                    <FormLabel>Events to send</FormLabel>
                    <FormDescription>
                      Select the events you want to receive at your webhook
                      endpoint
                    </FormDescription>
                  </div>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    {webhookEvents.map((event) => (
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
                              <FormLabel className="font-normal">
                                {event.label}
                              </FormLabel>
                            </FormItem>
                          )
                        }}
                      />
                    ))}
                  </div>
                  <FormMessage />
                </FormItem>
              )}
            />
          </form>
        </div>

        <div className="flex justify-end space-x-4 pt-4 border-t mt-6">
          <Button variant="outline" type="button" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            type="submit"
            disabled={disabled}
            onClick={form.handleSubmit(onSubmit)}
          >
            Save Webhook
          </Button>
        </div>
      </div>
    </Form>
  )
}
