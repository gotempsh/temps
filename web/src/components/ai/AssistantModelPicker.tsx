// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useIsMobile } from '@/components/hooks/use-mobile'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { Check, ChevronsUpDown, Loader2, TriangleAlert } from 'lucide-react'
import { useMemo, useState } from 'react'
import { AiProviderIcon } from '@/lib/ai-providers'
import { assistantKeyLabel, type AssistantKey } from './assistant-keys'
import {
  useAssistantProviderKeys,
  useEffectiveDefaultModel,
  useProviderModels,
  useSetDefaultModel,
} from './assistant-model-hooks'
import { toast } from 'sonner'
import {
  describeModel,
  filterAssistantModels,
  lacksToolCalling,
  modelDisplayName,
  type AssistantModelOption,
} from './assistant-models'
import { ModelVendorLogo } from './ModelVendorLogo'
import {
  featuredModels,
  filterByVendor,
  modelVendor,
  orderByPopularity,
  shortModelName,
  vendorOptions,
  type VendorOption,
} from './model-vendors'

/**
 * Searchable list of the key's models. Each row shows what goes in and out and
 * what it costs, so the choice does not need a trip to the provider's website.
 */
export function AssistantModelList({
  providerKeyId,
  value,
  defaultModelId,
  onChange,
  disabled,
  roomy,
}: {
  providerKeyId: number | null
  value: string | null
  /** Marked "Default" in the list. */
  defaultModelId?: string | null
  onChange: (modelId: string) => void
  disabled?: boolean
  roomy?: boolean
}) {
  const models = useProviderModels(providerKeyId)
  const [query, setQuery] = useState('')
  const [vendorKey, setVendorKey] = useState<string | null>(null)
  const ordered = useMemo(
    () => orderByPopularity(models.options),
    [models.options]
  )
  const vendors = useMemo(() => vendorOptions(ordered), [ordered])
  const visible = useMemo(
    () => filterAssistantModels(filterByVendor(ordered, vendorKey), query),
    [ordered, vendorKey, query]
  )
  // The popular group leads the unfiltered list; a vendor or a search is already
  // a narrower question than "what do people use".
  const popular = useMemo(
    () => (vendorKey === null && !query.trim() ? featuredModels(ordered) : []),
    [ordered, vendorKey, query]
  )
  const rest = useMemo(
    () => visible.filter((option) => !popular.includes(option)),
    [visible, popular]
  )
  const renderRow = (option: AssistantModelOption) => (
    <ModelRow
      key={option.id}
      option={option}
      selected={option.id === value}
      isDefault={option.id === defaultModelId}
      disabled={disabled}
      onSelect={() => onChange(option.id)}
    />
  )

  if (models.isPending && providerKeyId !== null) {
    return (
      <div className="space-y-2" role="status" aria-label="Loading models">
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-14 w-full" />
        <Skeleton className="h-14 w-full" />
        <Skeleton className="h-14 w-full" />
      </div>
    )
  }
  if (models.isError || models.options.length === 0) {
    return (
      <p className="text-sm text-muted-foreground" role="status">
        {models.isError
          ? getErrorMessage(
              models.error,
              'The model list could not be loaded. The provider default will be used.'
            )
          : 'This provider has not listed any models yet. The provider default will be used.'}
      </p>
    )
  }

  return (
    <div className="space-y-2">
      <div className="flex gap-2">
        <Input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={`Search ${models.options.length} models…`}
          aria-label="Search models"
          className={cn(
            'min-w-0 flex-1',
            roomy && 'h-11 text-base md:text-base'
          )}
          disabled={disabled}
        />
        <ModelVendorSelect
          vendors={vendors}
          total={models.options.length}
          value={vendorKey}
          onChange={setVendorKey}
          disabled={disabled}
          roomy={roomy}
        />
      </div>
      <ul
        className={cn(
          'max-h-[min(18rem,calc(var(--radix-popover-content-available-height,100vh)-5rem))] space-y-1 overflow-y-auto pr-1',
          roomy && 'max-h-80'
        )}
        aria-label="Models"
      >
        {visible.length === 0 ? (
          <li className="px-2 py-3 text-sm text-muted-foreground">
            No model matches “{query.trim()}”.
          </li>
        ) : (
          <>
            {popular.length > 0 ? (
              <ModelGroupHeading>Popular</ModelGroupHeading>
            ) : null}
            {popular.map(renderRow)}
            {popular.length > 0 ? (
              <ModelGroupHeading>All models</ModelGroupHeading>
            ) : null}
            {rest.map(renderRow)}
          </>
        )}
      </ul>
    </div>
  )
}

function ModelGroupHeading({ children }: { children: string }) {
  return (
    <li
      role="presentation"
      className="px-3 pb-1 pt-2 text-[11px] font-medium uppercase tracking-wide text-muted-foreground"
    >
      {children}
    </li>
  )
}

/** Value of the "every vendor" item: a Select item cannot be an empty string. */
const ALL_VENDORS = '__all'

/**
 * Narrows the list to one vendor. A dropdown rather than tabs: it fits a phone,
 * and it holds every vendor, not just the handful a row could show.
 */
export function ModelVendorSelect({
  vendors,
  total,
  value,
  onChange,
  disabled,
  roomy,
}: {
  vendors: readonly VendorOption[]
  total: number
  value: string | null
  onChange: (key: string | null) => void
  disabled?: boolean
  roomy?: boolean
}) {
  if (vendors.length < 2) return null
  return (
    <Select
      value={value ?? ALL_VENDORS}
      onValueChange={(next) => onChange(next === ALL_VENDORS ? null : next)}
      disabled={disabled}
    >
      <SelectTrigger
        aria-label="Filter models by vendor"
        className={cn(
          'w-36 shrink-0 gap-1.5 sm:w-44',
          roomy && 'h-11 text-base'
        )}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ALL_VENDORS}>
          All vendors <span className="tabular-nums opacity-60">{total}</span>
        </SelectItem>
        {vendors.map((vendor) => (
          <SelectItem key={vendor.key} value={vendor.key}>
            <span className="flex items-center gap-2">
              <ModelVendorLogo vendor={vendor} size={16} />
              {vendor.name}
              <span className="tabular-nums opacity-60">{vendor.count}</span>
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

function ModelRow({
  option,
  selected,
  isDefault,
  disabled,
  onSelect,
}: {
  option: AssistantModelOption
  selected: boolean
  isDefault: boolean
  disabled?: boolean
  onSelect: () => void
}) {
  const facts = describeModel(option)
  const vendor = modelVendor(option.id)
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        disabled={disabled}
        aria-pressed={selected}
        className={cn(
          'flex w-full items-start gap-2.5 rounded-md border border-transparent px-3 py-2 text-left transition-colors hover:bg-accent disabled:opacity-50',
          selected && 'border-border bg-muted'
        )}
      >
        <ModelVendorLogo vendor={vendor} size={20} className="mt-0.5" />
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="truncate text-sm font-medium">
              {shortModelName(option.name, option.id)}
            </span>
            {isDefault ? (
              <Badge
                variant="secondary"
                className="px-1.5 py-0 text-[11px] font-normal"
              >
                Default
              </Badge>
            ) : null}
            {lacksToolCalling(option) ? (
              <Badge
                variant="outline"
                className="gap-1 border-amber-500/50 px-1.5 py-0 text-[11px] font-normal text-amber-700 dark:text-amber-400"
              >
                <TriangleAlert className="size-3" aria-hidden="true" />
                No tool calling
              </Badge>
            ) : null}
          </span>
          {option.name !== option.id ? (
            <span className="hidden truncate font-mono text-xs text-muted-foreground sm:block">
              {option.id}
            </span>
          ) : null}
          {facts.length > 0 ? (
            <span className="mt-0.5 line-clamp-2 text-xs text-muted-foreground sm:line-clamp-none">
              {facts.join(' · ')}
            </span>
          ) : null}
        </span>
        {selected ? (
          <Check className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        ) : null}
      </button>
    </li>
  )
}

/**
 * Onboarding's second step, right after the key is saved: pick the model the
 * assistant starts on. Skipping keeps the provider's own default.
 */
export function AssistantModelSetup({
  providerKeyId,
  onDone,
  roomy,
}: {
  providerKeyId: number
  onDone: () => void
  roomy?: boolean
}) {
  const [choice, setChoice] = useState<string | null>(null)
  const setDefault = useSetDefaultModel(providerKeyId)
  const defaultModel = useEffectiveDefaultModel({ id: providerKeyId })

  const save = () => {
    if (choice === null) {
      onDone()
      return
    }
    setDefault.mutate(choice, {
      onSuccess: () => {
        toast.success('Default model saved')
        onDone()
      },
    })
  }

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <h3 className={cn('text-base font-semibold', roomy && 'text-xl')}>
          Choose the default model
        </h3>
        <p
          className={cn(
            'text-sm leading-6 text-muted-foreground',
            roomy && 'text-base leading-7'
          )}
        >
          New chats start on this model. The assistant calls the Temps API as it
          works, so pick one that supports tool calling. You can change it at
          any time, per chat or as the default.
        </p>
      </div>
      <AssistantModelList
        providerKeyId={providerKeyId}
        value={choice ?? defaultModel}
        defaultModelId={defaultModel}
        onChange={setChoice}
        disabled={setDefault.isPending}
        roomy={roomy}
      />
      {setDefault.isError ? (
        <p className="text-sm text-destructive" role="alert">
          {getErrorMessage(setDefault.error, 'Could not save the model.')}
        </p>
      ) : null}
      <div className="flex flex-wrap justify-end gap-2">
        <Button
          type="button"
          variant="ghost"
          onClick={onDone}
          disabled={setDefault.isPending}
          className={cn(roomy && 'h-11 text-base')}
        >
          Keep {defaultModel ?? 'the suggested model'}
        </Button>
        <Button
          type="button"
          onClick={save}
          disabled={choice === null || setDefault.isPending}
          className={cn(roomy && 'h-11 px-6 text-base')}
        >
          {setDefault.isPending ? (
            <Loader2 className="size-4 animate-spin" aria-hidden="true" />
          ) : null}
          Use this model
        </Button>
      </div>
    </div>
  )
}

/**
 * Which provider key the model list shows, for a new chat when more than one
 * key is active. Hidden with a single key: there is nothing to choose.
 */
export function AssistantKeySelect({
  keys,
  value,
  onChange,
  disabled,
}: {
  keys: readonly AssistantKey[]
  value: number | null
  onChange: (keyId: number) => void
  disabled?: boolean
}) {
  if (keys.length < 2) return null
  return (
    <div className="grid gap-1.5">
      <span className="text-xs font-medium text-muted-foreground">
        Provider
      </span>
      <Select
        value={value === null ? undefined : String(value)}
        onValueChange={(next) => onChange(Number(next))}
        disabled={disabled}
      >
        <SelectTrigger aria-label="Provider for this chat" className="w-full">
          <SelectValue placeholder="Choose a provider" />
        </SelectTrigger>
        <SelectContent>
          {keys.map((key) => (
            <SelectItem key={key.id} value={String(key.id)}>
              <span className="flex min-w-0 items-center gap-2">
                <AiProviderIcon provider={key.provider} size={16} />
                <span className="truncate">{assistantKeyLabel(key)}</span>
              </span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

/**
 * The model of one chat, as a button that opens the list. It is controlled by
 * the chat that owns it: choosing here never changes the key's default, so two
 * chats can run on different models.
 *
 * The chat may also choose its provider key (`chooseKey`) when more than one
 * is active; without it the list is the models of the chat's key
 * (`providerKeyId`).
 */
export function AssistantModelChip({
  value,
  onChange,
  providerKeyId,
  chooseKey,
  disabled,
  roomy,
}: {
  value: string | null
  onChange: (modelId: string, providerKeyId: number) => void
  /** The key the chat runs on; the default key when not given. */
  providerKeyId?: number | null
  /** Whether the provider key can be changed from this chip. */
  chooseKey?: boolean
  disabled?: boolean
  roomy?: boolean
}) {
  const { keys, defaultKey } = useAssistantProviderKeys()
  const currentKeyId = providerKeyId ?? defaultKey?.id ?? null
  const currentKey = keys.find((key) => key.id === currentKeyId) ?? null
  // The key whose models the open list shows; reset to the chat's own key
  // every time the list opens.
  const [browsingKeyId, setBrowsingKeyId] = useState<number | null>(null)
  const listKeyId = browsingKeyId ?? currentKeyId
  const models = useProviderModels(currentKeyId)
  const listKey = keys.find((key) => key.id === listKeyId) ?? null
  const listDefaultModel = useEffectiveDefaultModel(listKey)
  const [open, setOpen] = useState(false)
  const isMobile = useIsMobile()
  const showKeys = keys.length > 1

  const fullName = modelDisplayName(models.options, value)
  const label = fullName && value ? shortModelName(fullName, value) : 'Loading…'
  const keyLabel = showKeys && currentKey ? assistantKeyLabel(currentKey) : null

  const openChange = (next: boolean) => {
    setOpen(next)
    if (next) setBrowsingKeyId(null)
  }

  const choose = (modelId: string) => {
    setOpen(false)
    if (listKeyId === null) return
    if (modelId !== value || listKeyId !== currentKeyId) {
      onChange(modelId, listKeyId)
    }
  }

  return (
    <Popover open={open} onOpenChange={openChange}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={disabled}
          className={cn(
            'h-8 min-w-0 max-w-44 justify-between gap-2 px-2 font-normal text-muted-foreground hover:text-foreground @[30rem]:max-w-64 @[44rem]:max-w-96',
            roomy && 'h-9 text-sm'
          )}
          aria-label={`Model: ${label}${keyLabel ? ` on ${keyLabel}` : ''}. Change model for this chat`}
          title={
            keyLabel
              ? `The model for this chat, on ${keyLabel}`
              : 'The model for this chat'
          }
        >
          <span className="flex min-w-0 items-center gap-2">
            {value ? (
              <ModelVendorLogo vendor={modelVendor(value)} size={18} />
            ) : null}
            <span className="truncate font-medium text-foreground">
              {label}
            </span>
            {keyLabel ? (
              <span className="hidden truncate @[44rem]:inline">
                · {keyLabel}
              </span>
            ) : null}
          </span>
          <ChevronsUpDown
            className="size-4 shrink-0 opacity-50"
            aria-hidden="true"
          />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="grid w-[min(32rem,calc(100vw-1rem))] gap-3 p-3"
        onOpenAutoFocus={(event) => {
          // The keyboard would cover the list; search is one tap away.
          if (isMobile) event.preventDefault()
        }}
      >
        {chooseKey ? (
          <AssistantKeySelect
            keys={keys}
            value={listKeyId}
            onChange={setBrowsingKeyId}
          />
        ) : null}
        <AssistantModelList
          key={listKeyId ?? 'none'}
          providerKeyId={listKeyId}
          value={listKeyId === currentKeyId ? value : null}
          defaultModelId={listDefaultModel}
          onChange={choose}
          roomy={false}
        />
      </PopoverContent>
    </Popover>
  )
}
