// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CredentialProviderMark } from '@temps-sdk/ds'
import type { AccessServiceResponse } from '@/api/client'

export function ServiceProviderLogo({
  service,
}: {
  service: Pick<AccessServiceResponse, 'policy'>
}) {
  return (
    <CredentialProviderMark
      provider={
        service.policy.github_repository ? 'github' : service.policy.provider
      }
    />
  )
}
export function ServiceLogoStrip({
  services,
  total,
}: {
  services: AccessServiceResponse[]
  total: number
}) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2" aria-label="Services">
        {services.slice(0, 4).map((s) => (
          <span
            key={s.id}
            title={s.name}
            className="flex size-9 items-center justify-center rounded-md border"
          >
            <ServiceProviderLogo service={s} />
          </span>
        ))}
        {total > 4 && (
          <span className="flex size-9 items-center justify-center rounded-md border text-xs text-muted-foreground">
            +{total - 4}
          </span>
        )}
      </div>
      <p className="truncate text-xs text-muted-foreground">
        {services
          .slice(0, 4)
          .map((s) => s.name)
          .join(', ')}
        {total > 4 ? ` and ${total - 4} more` : ''}
      </p>
    </div>
  )
}
