// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createProviderKey,
  testProviderKeyInline,
  type TestProviderKeyRequest,
} from '@/api/client'
import { listProviderKeysQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { AiProviderIcon } from '@/lib/ai-providers-components'
import {
  isSelfHostedProvider,
  type AiProviderMeta,
} from '@/lib/ai-providers-shared'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, Loader2 } from 'lucide-react'
import { useState, type ChangeEvent } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import {
  ASSISTANT_PROVIDER_CHOICES,
  PROVIDER_KEY_PLACEHOLDER,
  providerTestRequest,
  providerTestSignature,
} from './assistant-providers'
import {
  providerKeyFormForProvider,
  providerKeyFormSchema,
  providerKeyFormToRequest,
  type ProviderKeyFormValues,
} from './provider-key-form'
import { SelfHostedProviderFields } from './SelfHostedProviderFields'

/** What a finished setup hands back. */
export interface SavedProvider {
  /** The stored key's id, when the server returned one. */
  keyId: number | null
  /** Whether the form already picked the model new chats start on. */
  modelChosen: boolean
}

const KEY_ID = 'assistant-provider-key'
const BASE_URL_ID = 'assistant-provider-base-url'

/**
 * Connects the assistant's first provider without leaving the screen: every
 * supported one is a choice, each with the whole of its form here. A vendor
 * takes its key; a server of the user's own takes its address, lists its
 * models and has one picked. Either can be tried before it is stored, and the
 * save verifies it again.
 */
export function AssistantProviderSetup({
  roomy,
  onSaved,
}: {
  /** Full-screen sizing: larger type and controls. */
  roomy?: boolean
  onSaved: (saved: SavedProvider) => void | Promise<void>
}) {
  const queryClient = useQueryClient()
  const canTrustPrivateAddress = useIsInstanceAdmin()
  const initial = providerKeyFormForProvider(ASSISTANT_PROVIDER_CHOICES[0], 0)
  const form = useForm<ProviderKeyFormValues>({
    resolver: zodResolver(providerKeyFormSchema),
    defaultValues: initial,
  })
  const values = useWatch({
    control: form.control,
    defaultValue: initial,
  }) as ProviderKeyFormValues
  const errors = form.formState.errors
  // The base URL as last left or pasted: with a key typed, the models are
  // listed only for that, never for a host still being typed.
  const [committedBaseUrl, setCommittedBaseUrl] = useState('')

  const provider =
    ASSISTANT_PROVIDER_CHOICES.find((meta) => meta.id === values.provider) ??
    ASSISTANT_PROVIDER_CHOICES[0]
  const selfHosted = isSelfHostedProvider(provider.id)
  const signature = providerTestSignature(values)

  const test = useMutation({
    mutationFn: async (attempt: {
      signature: string
      request: TestProviderKeyRequest
    }) => {
      const response = await testProviderKeyInline({
        body: attempt.request,
        throwOnError: true,
      })
      return response.data
    },
  })
  const save = useMutation({
    mutationFn: async (submitted: ProviderKeyFormValues) =>
      createProviderKey({
        body: providerKeyFormToRequest(submitted),
        throwOnError: true,
      }),
    onSuccess: async (result, submitted) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listProviderKeysQueryKey() }),
        queryClient.invalidateQueries({ queryKey: ['providerKeys'] }),
      ])
      toast.success(`${provider.name} connected`)
      await onSaved({
        keyId: result.data?.id ?? null,
        modelChosen: isSelfHostedProvider(submitted.provider),
      })
    },
  })
  const busy = save.isPending
  // A result stands only under the inputs it was made with.
  const tested = test.variables?.signature === signature
  const testing = test.isPending && tested

  const choose = (choice: AiProviderMeta) => {
    if (choice.id === provider.id) return
    // Nothing typed for one provider is sent to another, and the last one's
    // result does not stand under the new one.
    form.reset(providerKeyFormForProvider(choice, 0))
    setCommittedBaseUrl('')
    test.reset()
    save.reset()
  }
  const setValue = <K extends keyof ProviderKeyFormValues>(
    field: K,
    value: ProviderKeyFormValues[K]
  ) =>
    form.setValue(field, value as never, {
      shouldDirty: true,
      shouldValidate: form.formState.isSubmitted,
    })

  const fieldClass = cn(roomy && 'h-12 text-base md:text-base')
  const labelClass = cn('text-sm font-medium', roomy && 'text-base')
  const helpClass = cn(
    'text-xs leading-5 text-muted-foreground',
    roomy && 'text-sm leading-6'
  )
  const buttonClass = cn(roomy && 'h-12 px-6 text-base')

  return (
    <div className={cn('space-y-3', roomy && 'space-y-4')}>
      <div
        role="group"
        aria-label="AI provider"
        className={cn('grid grid-cols-2 gap-2', roomy && 'sm:grid-cols-3')}
      >
        {ASSISTANT_PROVIDER_CHOICES.map((choice) => {
          const selected = choice.id === provider.id
          return (
            <Button
              key={choice.id}
              type="button"
              variant="outline"
              aria-pressed={selected}
              disabled={busy}
              onClick={() => choose(choice)}
              className={cn(
                // Names wrap and never clip: the longest must read whole in
                // a narrow dock or on a phone.
                'h-auto min-w-0 justify-start gap-2 whitespace-normal px-2.5 py-2 text-left font-normal',
                roomy && 'py-2.5 sm:px-3 sm:text-base',
                selected &&
                  'border-foreground bg-accent font-medium text-accent-foreground'
              )}
            >
              <AiProviderIcon provider={choice.id} size={24} />
              <span className="min-w-0 [overflow-wrap:anywhere]">
                {choice.name}
              </span>
            </Button>
          )
        })}
      </div>

      <form
        noValidate
        onSubmit={form.handleSubmit((submitted) => save.mutate(submitted))}
        className={cn('space-y-3', roomy && 'space-y-4')}
      >
        {selfHosted ? (
          <div className="space-y-2">
            <label htmlFor={BASE_URL_ID} className={labelClass}>
              Base URL
            </label>
            <Input
              className={fieldClass}
              id={BASE_URL_ID}
              autoComplete="off"
              spellCheck={false}
              placeholder="https://api.example.com/v1"
              disabled={busy}
              aria-invalid={errors.base_url ? true : undefined}
              {...form.register('base_url', {
                onBlur: (event: ChangeEvent<HTMLInputElement>) =>
                  setCommittedBaseUrl(event.target.value),
                onChange: (event: ChangeEvent<HTMLInputElement>) => {
                  const { inputType } = event.nativeEvent as InputEvent
                  // A paste or browser autofill arrives whole.
                  if (
                    inputType === 'insertFromPaste' ||
                    inputType === 'insertReplacementText'
                  ) {
                    setCommittedBaseUrl(event.target.value)
                  }
                },
              })}
            />
            {errors.base_url ? (
              <p className="text-sm text-destructive" role="alert">
                {errors.base_url.message}
              </p>
            ) : null}
            <p className={helpClass}>
              The API root, including /v1. {provider.tagline}.
            </p>
          </div>
        ) : null}

        <div className="space-y-2">
          <label htmlFor={KEY_ID} className={labelClass}>
            {selfHosted ? (
              <>
                API key{' '}
                <span className="font-normal text-muted-foreground">
                  (optional)
                </span>
              </>
            ) : (
              `${provider.name} API key`
            )}
          </label>
          <Input
            className={fieldClass}
            id={KEY_ID}
            type="password"
            autoComplete="off"
            spellCheck={false}
            placeholder={
              selfHosted
                ? 'Only if the server needs one'
                : PROVIDER_KEY_PLACEHOLDER[provider.id]
            }
            disabled={busy}
            aria-invalid={errors.api_key ? true : undefined}
            {...form.register('api_key')}
          />
          {errors.api_key ? (
            <p className="text-sm text-destructive" role="alert">
              {errors.api_key.message}
            </p>
          ) : null}
          {selfHosted ? null : (
            <p className={helpClass}>
              {provider.tagline}. The key is checked with {provider.name}, then
              stored encrypted. It is never shown again.
              {provider.keyDocsUrl ? (
                <>
                  {' '}
                  <a
                    className="underline underline-offset-2"
                    href={provider.keyDocsUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Get a key
                  </a>
                </>
              ) : null}
            </p>
          )}
        </div>

        {/* Mounted for every provider, shown for a server of the user's own:
            the models it listed survive a look at another provider. */}
        <SelfHostedProviderFields
          open={selfHosted}
          baseUrl={values.base_url}
          committedBaseUrl={committedBaseUrl}
          apiKey={values.api_key}
          trustPrivateAddress={values.trust_private_address}
          onTrustPrivateAddressChange={(trusted) =>
            setValue('trust_private_address', trusted)
          }
          canTrustPrivateAddress={canTrustPrivateAddress}
          discovery={values.discovery}
          onDiscovered={(discovery) => setValue('discovery', discovery)}
          defaultModel={values.default_model}
          onDefaultModelChange={(model) => setValue('default_model', model)}
          capabilities={values.capabilities}
          onCapabilitiesChange={(capabilities) =>
            setValue('capabilities', capabilities)
          }
          error={errors.default_model?.message}
          disabled={busy}
        />

        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={busy} className={buttonClass}>
            {busy ? (
              <Loader2 className="size-4 animate-spin" aria-hidden="true" />
            ) : null}
            Connect
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={busy || testing}
            className={buttonClass}
            onClick={form.handleSubmit((submitted) =>
              test.mutate({
                signature: providerTestSignature(submitted),
                request: providerTestRequest(submitted),
              })
            )}
          >
            {testing ? (
              <Loader2 className="size-4 animate-spin" aria-hidden="true" />
            ) : null}
            Test
          </Button>
        </div>

        {tested && test.data?.success ? (
          <p
            role="status"
            className={cn(
              'flex items-start gap-1.5 text-sm',
              roomy && 'text-base'
            )}
          >
            <CircleCheck
              className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400"
              aria-hidden="true"
            />
            {provider.name} answered in {test.data.latency_ms} ms. Nothing is
            stored until you connect.
          </p>
        ) : null}
        {tested && test.data && !test.data.success ? (
          <p className="text-sm text-destructive" role="alert">
            {test.data.error ?? `${provider.name} refused the test request.`}
          </p>
        ) : null}
        {tested && test.isError ? (
          <p className="text-sm text-destructive" role="alert">
            {getErrorMessage(test.error, 'Could not run the test.')}
          </p>
        ) : null}
        {save.isError ? (
          <p className="text-sm text-destructive" role="alert">
            {getErrorMessage(
              save.error,
              `${provider.name} could not be connected. Check what you entered and try again.`
            )}
          </p>
        ) : null}
      </form>
    </div>
  )
}
