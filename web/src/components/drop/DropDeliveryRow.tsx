// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useDeliveryProjectCapability } from '@/components/domains/delivery-capability'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  DROP_DELIVERY_LABELS,
  dropDeliveryChoices,
  effectiveDropDeliveryDefault,
  type DropDeliveryChoice,
} from '@/lib/drop-delivery'
import { ExternalLink } from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'

/**
 * Compact delivery summary for the drop card. A provider is only selectable
 * once it has been set up; otherwise the row says so and links to the setup
 * page in a new tab, so the dropped files are not lost.
 */
export function DropDeliveryRow({
  value,
  onChange,
  disabled = false,
}: {
  value: DropDeliveryChoice | undefined
  onChange: (value: DropDeliveryChoice | undefined) => void
  disabled?: boolean
}) {
  const capability = useDeliveryProjectCapability()
  const choices = dropDeliveryChoices(capability.data)
  const selected = value ?? effectiveDropDeliveryDefault(capability.data)
  const overrideUnavailable =
    capability.isSuccess && value !== undefined && !choices.includes(value)

  // A provider can stop being ready while the card is open. Sending it as an
  // explicit choice would fail project creation, so fall back to the default.
  useEffect(() => {
    if (overrideUnavailable) onChange(undefined)
  }, [overrideUnavailable, onChange])

  return (
    <div className="mt-3 flex items-center justify-between gap-3">
      <span className="text-muted-foreground">CDN delivery</span>
      {capability.isPending ? (
        <Skeleton className="h-4 w-20" />
      ) : capability.isError ? (
        <span className="font-medium">
          {value ? DROP_DELIVERY_LABELS[value] : 'Project default'}
        </span>
      ) : choices.length > 1 ? (
        <Select
          value={selected}
          onValueChange={(next) => onChange(next as DropDeliveryChoice)}
          disabled={disabled}
        >
          <SelectTrigger
            aria-label="CDN delivery"
            className="-my-1 h-7 w-auto min-w-[8.5rem] bg-background"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent align="end">
            {choices.map((choice) => (
              <SelectItem key={choice} value={choice}>
                {DROP_DELIVERY_LABELS[choice]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      ) : (
        <span className="flex items-center gap-2">
          <span className="font-medium">None</span>
          <Link
            to="/delivery-profiles"
            target="_blank"
            rel="noreferrer"
            title="Opens in a new tab so your dropped files stay here"
            className="inline-flex items-center gap-1 text-xs text-muted-foreground underline hover:text-foreground"
          >
            Set up a CDN
            <ExternalLink className="size-3" aria-hidden="true" />
          </Link>
        </span>
      )}
    </div>
  )
}
