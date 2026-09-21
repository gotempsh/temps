// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { KeyRound } from 'lucide-react'
import { credentialProviderAssets } from './credential-provider-assets'
import { cn } from './lib/cn'

/** Exact canonical provider IDs only. Never pass an env-var name or hostname. */
export function CredentialProviderMark({
  provider,
  className,
}: {
  provider?: string | null
  className?: string
}) {
  const asset =
    provider &&
    Object.prototype.hasOwnProperty.call(credentialProviderAssets, provider)
      ? credentialProviderAssets[
          provider as keyof typeof credentialProviderAssets
        ]
      : undefined
  return (
    <span
      className={cn(
        'inline-flex size-6 shrink-0 items-center justify-center',
        className
      )}
    >
      {asset ? (
        <>
          <img
            src={asset.src}
            alt={asset.name}
            width={24}
            height={24}
            className={cn(
              'size-full object-contain',
              asset.darkSrc && 'dark:hidden',
              asset.invertInDark && 'dark:invert'
            )}
          />
          {asset.darkSrc && (
            <img
              src={asset.darkSrc}
              alt={asset.name}
              width={24}
              height={24}
              className="hidden size-full object-contain dark:block"
            />
          )}
        </>
      ) : (
        <KeyRound
          className="size-4 text-muted-foreground"
          aria-label="Custom or unknown provider"
          role="img"
        />
      )}
    </span>
  )
}
