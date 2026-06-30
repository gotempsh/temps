'use client'

import { Button } from '@/components/ui/button'
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
import { ProviderFormData } from './schemas'

interface ProviderFormProps {
  form: UseFormReturn<ProviderFormData>
  onSubmit: (data: ProviderFormData) => Promise<void>
  isEdit?: boolean
  isLoading?: boolean
  formId?: string
  hideSubmit?: boolean
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
  formId,
  hideSubmit = false,
}: ProviderFormProps) {
  const providerType = form.watch('provider_type')
  const tlsMode = form.watch('config.tls_mode')

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
        id={formId}
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
                      <SelectItem value="webhook">Webhook</SelectItem>
                      <SelectItem value="cloudflare">
                        Cloudflare Email
                      </SelectItem>
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
                    onValueChange={(value) =>
                      field.onChange(value || undefined)
                    }
                    value={field.value}
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

        {providerType === 'webhook' && (
          <div className="space-y-6">
            <div className="space-y-4">
              <h3 className="text-sm font-medium leading-none">
                Webhook Configuration
              </h3>
              <p className="text-sm text-muted-foreground">
                Send notifications as JSON payloads to any HTTP endpoint. Use custom headers for authentication.
              </p>
            </div>

            <FormField
              control={form.control}
              name="config.url"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Webhook URL</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="https://api.example.com/webhook"
                    />
                  </FormControl>
                  <FormDescription>
                    The URL where notifications will be sent as JSON payloads
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <FormField
                control={form.control}
                name="config.method"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>HTTP Method</FormLabel>
                    <Select
                      onValueChange={field.onChange}
                      value={field.value || 'POST'}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select method" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="POST">POST</SelectItem>
                        <SelectItem value="PUT">PUT</SelectItem>
                        <SelectItem value="PATCH">PATCH</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="config.timeout_secs"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Timeout (seconds)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        placeholder="30"
                        min={1}
                        max={300}
                        onChange={(e) =>
                          field.onChange(
                            e.target.value ? parseInt(e.target.value) : 30
                          )
                        }
                        value={field.value || 30}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            <div className="space-y-4">
              <h4 className="text-sm font-medium leading-none">
                Custom Headers
              </h4>
              <p className="text-sm text-muted-foreground">
                Add custom headers for authentication (e.g., Authorization: Bearer token)
              </p>

              <FormField
                control={form.control}
                name="config.headers"
                render={({ field }) => {
                  const headers = field.value || {}
                  const headerEntries = Object.entries(headers)

                  const addHeader = () => {
                    field.onChange({ ...headers, '': '' })
                  }

                  const updateHeader = (oldKey: string, newKey: string, value: string) => {
                    const newHeaders = { ...headers }
                    if (oldKey !== newKey) {
                      delete newHeaders[oldKey]
                    }
                    if (newKey) {
                      newHeaders[newKey] = value
                    }
                    field.onChange(newHeaders)
                  }

                  const removeHeader = (key: string) => {
                    const newHeaders = { ...headers }
                    delete newHeaders[key]
                    field.onChange(newHeaders)
                  }

                  return (
                    <FormItem>
                      <div className="space-y-2">
                        {headerEntries.map(([key, value], index) => (
                          <div key={index} className="flex gap-2 items-center">
                            <Input
                              placeholder="Header name"
                              value={key}
                              onChange={(e) => updateHeader(key, e.target.value, value as string)}
                              className="flex-1"
                            />
                            <Input
                              placeholder="Header value"
                              value={value as string}
                              onChange={(e) => updateHeader(key, key, e.target.value)}
                              className="flex-1"
                            />
                            <Button
                              type="button"
                              variant="outline"
                              size="sm"
                              onClick={() => removeHeader(key)}
                            >
                              Remove
                            </Button>
                          </div>
                        ))}
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={addHeader}
                        >
                          Add Header
                        </Button>
                      </div>
                      <FormDescription>
                        Common headers: Authorization, X-API-Key, X-Custom-Header
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )
                }}
              />
            </div>

            <div className="rounded-lg border p-4 bg-muted/50">
              <h4 className="text-sm font-medium mb-2">Webhook Payload Format</h4>
              <pre className="text-xs text-muted-foreground overflow-x-auto">
{`{
  "id": "notification-uuid",
  "title": "Alert Title",
  "message": "Alert message content",
  "type": "error | warning | info | alert",
  "priority": "critical | high | normal | low",
  "severity": "critical | warning | info",
  "timestamp": "2025-01-01T12:00:00Z",
  "metadata": { "key": "value" }
}`}
              </pre>
            </div>
          </div>
        )}

        {providerType === 'cloudflare' && (
          <div className="space-y-6">
            <div className="space-y-4">
              <h3 className="text-sm font-medium leading-none">
                Cloudflare Email Sending
              </h3>
              <p className="text-sm text-muted-foreground">
                Sends notification emails through Cloudflare&apos;s
                transactional Email Sending API. The sender domain must be
                configured for Email Sending in your Cloudflare account.
              </p>
            </div>

            <FormField
              control={form.control}
              name="config.account_id"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Account ID</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="023e105f4ecef8ad9ca31a8372d0c353"
                    />
                  </FormControl>
                  <FormDescription>
                    Your Cloudflare account identifier
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="config.api_token"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>API Token</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      type="password"
                      placeholder="••••••••"
                      autoComplete="off"
                    />
                  </FormControl>
                  <FormDescription>
                    A Cloudflare API token with the Email Sending permission
                  </FormDescription>
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
                      placeholder="welcome@infracf.example.com"
                    />
                  </FormControl>
                  <FormDescription>
                    Must belong to a domain enabled for Cloudflare Email Sending
                  </FormDescription>
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

        {!hideSubmit && (
          <div className="flex justify-end pt-2">
            <Button type="submit" disabled={isLoading}>
              {isLoading
                ? 'Saving...'
                : isEdit
                  ? 'Update Provider'
                  : 'Add Provider'}
            </Button>
          </div>
        )}
      </form>
    </Form>
  )
}
