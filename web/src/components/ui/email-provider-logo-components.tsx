// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import {
  emailProviderConfig,
  type EmailProviderLogoProps,
} from './email-provider-logo-shared'

export function EmailProviderLogo({
  provider,
  size = 24,
  showLabel = false,
  className,
  ...props
}: EmailProviderLogoProps) {
  const config = emailProviderConfig[provider]

  if (!config) return null

  const Icon = config.icon

  return (
    <div className={cn('flex items-center gap-2', className)} {...props}>
      <Icon className={config.color} width={size} height={size} />
      {showLabel && <span className="font-medium">{config.label}</span>}
    </div>
  )
}
