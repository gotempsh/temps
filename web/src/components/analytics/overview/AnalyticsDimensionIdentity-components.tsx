// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  FileText,
  Languages,
  MapPin,
  MousePointerClick,
  Megaphone,
  Radio,
} from 'lucide-react'
import { TechnologyIcon } from './TechnologyIcon'
import { ReferrerIcon } from './ReferrerIdentity'
import {
  countryFlag,
  dimensionLabel,
} from './AnalyticsDimensionIdentity-shared'

export function AnalyticsDimensionIcon({
  dimension,
  value,
}: {
  dimension?: string
  value: string
}) {
  if (dimension === 'referrer_hostname')
    return <ReferrerIcon key={value} domain={value} className="size-5" />
  if (dimension === 'language')
    return <Languages className="size-4 text-muted-foreground" />
  if (dimension === 'country') {
    const flag = countryFlag(value)
    return flag ? (
      <span role="img" aria-label={dimensionLabel(dimension, value)}>
        {flag}
      </span>
    ) : (
      <MapPin className="size-4 text-muted-foreground" />
    )
  }
  if (['browser', 'operating_system', 'device_type'].includes(dimension ?? ''))
    return <TechnologyIcon dimension={dimension} value={value} />
  if (dimension === 'channel')
    return <Radio className="size-4 text-muted-foreground" />
  if (dimension === 'utm_campaign')
    return <Megaphone className="size-4 text-muted-foreground" />
  if (dimension === 'region' || dimension === 'city')
    return <MapPin className="size-4 text-muted-foreground" />
  if (dimension?.includes('event'))
    return <MousePointerClick className="size-4 text-muted-foreground" />
  return <FileText className="size-4 text-muted-foreground" />
}
