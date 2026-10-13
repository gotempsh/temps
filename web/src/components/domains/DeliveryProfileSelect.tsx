// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listDeliveryProfiles } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { useDebounce } from '@/hooks/useDebounce'
import { cn } from '@/lib/utils'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { Check, ChevronsUpDown } from 'lucide-react'
import { type Ref, useState } from 'react'
import { Link } from 'react-router'
import { deliveryError, requireDeliveryData } from './delivery-errors'
import {
  DELIVERY_PROFILE_PICKER_QUERY,
  DELIVERY_PROFILES_QUERY_ROOT,
  deliveryProfileOptionLabel,
  isDeliveryProfileListTruncated,
  type DeliveryProfileListing,
  type DeliveryProfileOption,
} from './delivery-queries'

const INHERIT_PROFILE = 'inherit-profile'

/**
 * Pick a delivery profile, or `inheritLabel` for none (the empty value).
 *
 * With an empty search it lists `profiles`: the first page by name plus any
 * saved choice outside it. Typing searches every profile on the server, so a
 * profile beyond the first page can still be chosen.
 */
export function DeliveryProfileSelect({
  profiles,
  value,
  onChange,
  inheritLabel,
  disabled,
  id,
  'aria-describedby': ariaDescribedBy,
  'aria-invalid': ariaInvalid,
  triggerRef,
}: {
  profiles: DeliveryProfileOption[]
  value: string
  onChange: (value: string) => void
  inheritLabel: string
  disabled?: boolean
  id?: string
  'aria-describedby'?: string
  'aria-invalid'?: boolean
  triggerRef?: Ref<HTMLButtonElement>
}) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  // A profile chosen from search results keeps its name on the trigger after
  // the search is cleared, although it is not in `profiles`.
  const [chosen, setChosen] = useState<DeliveryProfileOption | null>(null)
  const term = useDebounce(search.trim(), 250)
  const searching = term !== ''
  const query = { ...DELIVERY_PROFILE_PICKER_QUERY, search: term }
  const results = useQuery({
    queryKey: [DELIVERY_PROFILES_QUERY_ROOT, 'list', query],
    queryFn: async () =>
      requireDeliveryData(await listDeliveryProfiles({ query })),
    enabled: open && searching,
    placeholderData: keepPreviousData,
  })
  const options = searching ? (results.data?.items ?? []) : profiles
  const selected =
    value === ''
      ? undefined
      : [...profiles, ...(chosen ? [chosen] : []), ...options].find(
          (profile) => String(profile.id) === value
        )
  const selectedLabel = selected
    ? deliveryProfileOptionLabel(selected)
    : `Profile #${value}`
  // A saved choice outside the listed page still needs a row showing it.
  const unlistedSelection =
    !searching &&
    value !== '' &&
    !profiles.some((profile) => String(profile.id) === value)
  const unshownMatches =
    searching && results.data
      ? results.data.total - results.data.items.length
      : 0

  const close = () => {
    setOpen(false)
    setSearch('')
  }
  const choose = (profile: DeliveryProfileOption | null) => {
    if (profile) setChosen(profile)
    onChange(profile ? String(profile.id) : '')
    close()
  }

  return (
    <Popover
      open={open}
      onOpenChange={(next) => (next ? setOpen(true) : close())}
    >
      <PopoverTrigger asChild>
        <Button
          ref={triggerRef}
          id={id}
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-describedby={ariaDescribedBy}
          aria-invalid={ariaInvalid}
          disabled={disabled}
          className="w-full justify-between font-normal"
        >
          <span className="truncate">
            {value === '' ? inheritLabel : selectedLabel}
          </span>
          <ChevronsUpDown className="ml-2 size-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-[min(calc(100vw-2rem),360px)] min-w-[var(--radix-popover-trigger-width)] p-0"
      >
        <Command shouldFilter={false}>
          <CommandInput
            value={search}
            onValueChange={setSearch}
            placeholder="Search delivery profiles..."
          />
          <CommandList>
            <CommandEmpty>
              {results.isError
                ? deliveryError(results.error)
                : results.isFetching
                  ? 'Searching...'
                  : 'No delivery profile matches.'}
            </CommandEmpty>
            <CommandGroup>
              {!searching && (
                <CommandItem
                  value={INHERIT_PROFILE}
                  onSelect={() => choose(null)}
                >
                  <SelectedMark selected={value === ''} />
                  {inheritLabel}
                </CommandItem>
              )}
              {options.map((profile) => (
                <CommandItem
                  key={profile.id}
                  value={String(profile.id)}
                  onSelect={() => choose(profile)}
                >
                  <SelectedMark selected={String(profile.id) === value} />
                  {deliveryProfileOptionLabel(profile)}
                </CommandItem>
              ))}
              {unlistedSelection && (
                <CommandItem value={value} onSelect={close}>
                  <SelectedMark selected />
                  {selectedLabel}
                </CommandItem>
              )}
            </CommandGroup>
          </CommandList>
          {unshownMatches > 0 && (
            <p className="border-t px-3 py-2 text-xs text-muted-foreground">
              {results.data?.total} profiles match; the first{' '}
              {results.data?.items.length} are shown. Keep typing to narrow the
              list.
            </p>
          )}
        </Command>
      </PopoverContent>
    </Popover>
  )
}

function SelectedMark({ selected }: { selected: boolean }) {
  return (
    <Check
      className={cn('mr-2 size-4', selected ? 'opacity-100' : 'opacity-0')}
    />
  )
}

/**
 * Says when a picker lists only the first page of profiles, so a missing
 * profile reads as "search for it" rather than "does not exist".
 */
export function DeliveryProfileLimitNote({
  listing,
}: {
  listing: DeliveryProfileListing | undefined
}) {
  if (!listing || !isDeliveryProfileListTruncated(listing)) return null
  return (
    <p className="text-xs text-muted-foreground">
      The picker lists the first {listing.items.length} of {listing.total}{' '}
      delivery profiles by name; search in it to find the others.{' '}
      <Link className="underline" to="/delivery-profiles">
        View all profiles
      </Link>
    </p>
  )
}
