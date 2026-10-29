// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// One field a plan option still needs answered, rendered from its ADR 058
// D8 descriptor, so a new provider needs no console code.

import { useQuery } from '@tanstack/react-query'
import type { Control } from 'react-hook-form'
import { Link, useLocation } from 'react-router'
import type { SetupField } from '@/api/client'
import { listDnsProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import { CloudflareTokenCheck } from '@/components/domains/CloudflareTokenGuide'
import { PeoplePicker } from '@/components/reachability/PeoplePicker'
import {
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { dnsProviderSetupPath, dnsProviderTypeLabel } from '@/lib/reachability'
import type { ReachabilityFormValues } from './reachability-form'

export function SetupFieldInput({
  field: setupField,
  control,
  address,
  token,
}: {
  field: SetupField
  control: Control<ReachabilityFormValues>
  /** The address being planned: the token check finds its zone. */
  address: string
  /** The token typed so far, when this is the token field. */
  token: string
}) {
  const providerType =
    setupField.kind.type === 'dns_provider'
      ? setupField.kind.provider_type
      : null
  const providers = useQuery({
    ...listDnsProvidersOptions(),
    enabled: setupField.kind.type === 'dns_provider',
  })
  const choices = (providers.data ?? []).filter(
    (p) => p.is_active && (!providerType || p.provider_type === providerType)
  )
  const location = useLocation()
  return (
    <FormField
      control={control}
      name={`values.${setupField.key}`}
      render={({ field }) => (
        <FormItem>
          <FormLabel>
            {setupField.label}
            {!setupField.required && (
              <span className="ml-1 font-normal text-muted-foreground">
                (optional)
              </span>
            )}
          </FormLabel>
          {setupField.kind.type === 'teams' ||
          setupField.kind.type === 'users' ? (
            <PeoplePicker
              kind={setupField.kind.type}
              value={field.value ?? ''}
              onChange={field.onChange}
            />
          ) : setupField.kind.type === 'choice' ? (
            <FormControl>
              <RadioGroup
                aria-label={setupField.label}
                value={field.value ?? ''}
                onValueChange={field.onChange}
                className="grid gap-2"
              >
                {setupField.kind.choices.map((choice) => (
                  <label
                    key={choice.value}
                    className="flex cursor-pointer items-start gap-2 text-sm"
                  >
                    <RadioGroupItem value={choice.value} className="mt-0.5" />
                    <span className="space-y-0.5">
                      <span className="block font-medium">{choice.label}</span>
                      {choice.help && (
                        <span className="block text-xs text-muted-foreground">
                          {choice.help}
                        </span>
                      )}
                    </span>
                  </label>
                ))}
              </RadioGroup>
            </FormControl>
          ) : setupField.kind.type === 'text_list' ? (
            <FormControl>
              <Textarea
                rows={3}
                placeholder={setupField.placeholder ?? undefined}
                {...field}
                value={field.value ?? ''}
              />
            </FormControl>
          ) : setupField.kind.type === 'dns_provider' ? (
            providers.isPending ? (
              <Skeleton className="h-9 w-full" />
            ) : providers.isError ? (
              <Alert variant="destructive">
                <AlertDescription className="space-y-2">
                  <p>
                    Temps could not load your DNS providers, so it cannot tell
                    which accounts you can choose.
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => void providers.refetch()}
                  >
                    Retry
                  </Button>
                </AlertDescription>
              </Alert>
            ) : choices.length === 0 ? (
              <Alert>
                <AlertDescription>
                  <p>
                    No active{' '}
                    {[dnsProviderTypeLabel(providerType), 'DNS provider']
                      .filter(Boolean)
                      .join(' ')}{' '}
                    is connected, so there is no account to choose.{' '}
                    <Link
                      className="underline"
                      to={dnsProviderSetupPath(
                        providerType,
                        `${location.pathname}${location.search}`
                      )}
                    >
                      Add one
                    </Link>
                    ; you come back here once it is added.
                  </p>
                </AlertDescription>
              </Alert>
            ) : (
              <Select value={field.value ?? ''} onValueChange={field.onChange}>
                <FormControl>
                  <SelectTrigger>
                    <SelectValue placeholder="Choose an account" />
                  </SelectTrigger>
                </FormControl>
                <SelectContent>
                  {choices.map((provider) => (
                    <SelectItem key={provider.id} value={String(provider.id)}>
                      {provider.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )
          ) : (
            <FormControl>
              <Input
                type={setupField.kind.type === 'secret' ? 'password' : 'text'}
                autoComplete="off"
                placeholder={setupField.placeholder ?? undefined}
                {...field}
                value={field.value ?? ''}
              />
            </FormControl>
          )}
          {setupField.help && (
            <FormDescription>{setupField.help}</FormDescription>
          )}
          {setupField.key === 'cloudflare_api_token' && (
            <CloudflareTokenCheck token={token} hostname={address} />
          )}
          <FormMessage />
        </FormItem>
      )}
    />
  )
}
