'use client'

import { Button } from '@/components/ui/button'
import { DialogFooter } from '@/components/ui/dialog'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { UseFormReturn } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

// Move the schema here since it's specific to the provider form
export const providerSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  provider_type: z.enum(['email', 'slack']),
  config: z
    .object({
      // Slack config
      webhook_url: z.string().url('Invalid webhook URL').optional(),
      channel: z.string().optional(),
      slack_username: z.string().optional(),

      // Email config
      smtp_host: z.string().optional(),
      smtp_port: z.number().min(1).max(65535).optional(),
      use_credentials: z.boolean().optional(),
      smtp_username: z.string().optional(),
      password: z.string().optional(),
      from_name: z.string().optional(),
      from_address: z.string().email('Invalid from address').optional(),
      to_addresses: z
        .array(z.string().email('Invalid email address'))
        .optional(),
      tls_mode: z
        .union([
          z.enum(['None', 'Starttls', 'Tls']),
          z.literal(''),
          z.undefined(),
          z.null(),
        ])
        .transform((val) => (val === '' || val === null ? undefined : val))
        .optional(),
      starttls_required: z.boolean().optional(),
      accept_invalid_certs: z.boolean().optional(),
    })
    .refine((data) => {
      if (data.webhook_url) return true
      // Username and password are now optional for SMTP
      if (
        data.smtp_host &&
        data.smtp_port &&
        data.from_address &&
        data.to_addresses
      )
        return true
      return false
    }, 'Please fill in all required fields for the selected provider type'),
})

export type ProviderFormData = z.infer<typeof providerSchema>

interface ProviderFormProps {
  form: UseFormReturn<ProviderFormData>
  onSubmit: (data: ProviderFormData) => Promise<void>
  isEdit?: boolean
  isLoading?: boolean
}

const showToastFormError = (error: any) => {
  toast.error(
    `The form has errors, please check the fields and try again: ${JSON.stringify(error)}`
  )
}

export function ProviderForm({
  form,
  onSubmit,
  isEdit = false,
  isLoading = false,
}: ProviderFormProps) {
  const providerType = form.watch('provider_type')
  const tlsMode = form.watch('config.tls_mode')
  const useCredentials = form.watch('config.use_credentials')

  // Suggest port based on TLS mode
  const getSuggestedPort = () => {
    switch (tlsMode) {
      case 'None':
        return '25'
      case 'Tls':
        return '465'
      case 'Starttls':
      default:
        return '587'
    }
  }

  return (
    <Form {...form}>
      <form
        onSubmit={form.handleSubmit(onSubmit, showToastFormError)}
        className="space-y-4 py-4"
      >
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Name</FormLabel>
                <FormControl>
                  <Input {...field} placeholder="My Provider" />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />

          {!isEdit && (
            <FormField
              control={form.control}
              name="provider_type"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Provider Type</FormLabel>
                  <Select onValueChange={field.onChange} value={field.value}>
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      <SelectItem value="email">Email</SelectItem>
                      <SelectItem value="slack">Slack</SelectItem>
                    </SelectContent>
                  </Select>
                  <FormMessage />
                </FormItem>
              )}
            />
          )}
        </div>

        {providerType === 'email' && (
          <div className="space-y-6">
            {/* Server Configuration Section */}
            <div className="space-y-4">
              <h3 className="text-sm font-medium leading-none">
                Server Configuration
              </h3>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <FormField
                  control={form.control}
                  name="config.smtp_host"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>SMTP Host</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="smtp.example.com" />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="config.smtp_port"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>SMTP Port</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          placeholder={getSuggestedPort()}
                          onChange={(e) =>
                            field.onChange(
                              e.target.value ? parseInt(e.target.value) : ''
                            )
                          }
                        />
                      </FormControl>
                      <FormDescription>
                        Common ports: 25 (unencrypted), 587 (STARTTLS), 465
                        (TLS)
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            </div>

            <FormField
              control={form.control}
              name="config.tls_mode"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>TLS Mode</FormLabel>
                  <Select
                    onValueChange={field.onChange}
                    value={field.value || undefined}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue placeholder="Select TLS mode" />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      <SelectItem value="None">None (No encryption)</SelectItem>
                      <SelectItem value="Starttls">
                        STARTTLS (Opportunistic TLS)
                      </SelectItem>
                      <SelectItem value="Tls">
                        TLS (Direct TLS connection)
                      </SelectItem>
                    </SelectContent>
                  </Select>
                  <FormDescription>
                    Select the encryption method for SMTP connection
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.starttls_required"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Require STARTTLS
                    </FormLabel>
                    <FormDescription>
                      Enforce STARTTLS encryption when using STARTTLS mode
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value ?? false}
                      onCheckedChange={field.onChange}
                      disabled={form.watch('config.tls_mode') !== 'Starttls'}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.accept_invalid_certs"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Accept Invalid Certificates
                    </FormLabel>
                    <FormDescription>
                      Allow connection to SMTP servers with self-signed or
                      invalid SSL/TLS certificates
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value ?? false}
                      onCheckedChange={field.onChange}
                      disabled={form.watch('config.tls_mode') === 'None'}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.use_credentials"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4 md:col-span-2">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Use Authentication
                    </FormLabel>
                    <FormDescription>
                      Enable SMTP authentication with username and password
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value ?? false}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.smtp_username"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>SMTP Username</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="username"
                      disabled={!form.watch('config.use_credentials')}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.password"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>SMTP Password</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      type="password"
                      placeholder="••••••••"
                      disabled={!form.watch('config.use_credentials')}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.from_name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>From Name</FormLabel>
                  <FormControl>
                    <Input {...field} placeholder="Notification System" />
                  </FormControl>
                  <FormDescription>
                    The name that will appear in the email sender field
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.from_address"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>From Address</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      type="email"
                      placeholder="notifications@example.com"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.to_addresses"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>To Addresses</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="recipient1@example.com, recipient2@example.com"
                      onChange={(e) =>
                        field.onChange(
                          e.target.value.split(',').map((email) => email.trim())
                        )
                      }
                      value={field.value?.join(', ') || ''}
                    />
                  </FormControl>
                  <FormDescription>
                    Separate multiple email addresses with commas
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
          </div>
        )}

        {providerType === 'slack' && (
          <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
            <div className="md:col-span-2">
              <FormField
                control={form.control}
                name="config.webhook_url"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Webhook URL</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        placeholder="https://hooks.slack.com/..."
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            <FormField
              control={form.control}
              name="config.channel"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Channel (Optional)</FormLabel>
                  <FormControl>
                    <Input {...field} placeholder="#notifications" />
                  </FormControl>
                  <FormDescription>
                    Override the default channel from the webhook
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
          </div>
        )}

        <DialogFooter className="shrink-0">
          <Button type="submit" disabled={isLoading}>
            {isLoading
              ? 'Saving...'
              : isEdit
                ? 'Update Provider'
                : 'Add Provider'}
          </Button>
        </DialogFooter>
      </form>
    </Form>
  )
}
