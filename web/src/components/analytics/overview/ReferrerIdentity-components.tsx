// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Globe, Link } from 'lucide-react'
import { type ReferrerIconProps } from './ReferrerIdentity-shared'

export function ReferrerIcon({
  domain,
  className = 'h-5 w-5',
}: ReferrerIconProps) {
  const [hasError, setHasError] = React.useState(false)

  if (!domain || domain === 'Direct') {
    return <Link className={`${className} text-muted-foreground`} />
  }

  if (hasError) {
    return <Globe className={`${className} text-muted-foreground`} />
  }

  const faviconDomain = ['twitter.com', 't.co'].includes(domain)
    ? 'x.com'
    : domain
  const faviconUrl = `https://www.google.com/s2/favicons?domain=${encodeURIComponent(faviconDomain)}&sz=32`

  return (
    <img
      src={faviconUrl}
      alt={`${domain} favicon`}
      className={`${className} rounded-sm bg-white object-contain`}
      onError={() => setHasError(true)}
    />
  )
}
