// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ProjectSecretResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { CredentialDetails } from './CredentialDetails'

export function SecretDetails({
  projectId,
  secret,
  detailPath,
}: {
  projectId: number
  secret: ProjectSecretResponse
  detailPath: string
}) {
  const services = secret.compose_services ?? []
  return (
    <CredentialDetails
      projectId={projectId}
      subject={{ kind: 'secret', id: secret.id, key: secret.key }}
      detailPath={detailPath}
      createdAt={secret.created_at}
      badge={<Badge variant="secondary">Secret file</Badge>}
      facts={
        <>
          <div>
            <dt className="text-muted-foreground">Value</dt>
            <dd className="mt-1 font-mono">••••••••••••</dd>
            <dd className="text-xs text-muted-foreground break-all">
              Write-only · mounted at /run/secrets/{secret.key}
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Scope</dt>
            <dd className="mt-1 flex flex-wrap gap-1">
              {secret.environments.length
                ? secret.environments.map((env) => (
                    <Badge key={env.id} variant="outline">
                      {env.name}
                    </Badge>
                  ))
                : 'All environments'}
            </dd>
            <dd className="text-xs text-muted-foreground">
              {[
                secret.include_in_preview ? 'Preview enabled' : null,
                services.length ? `Only ${services.join(', ')}` : null,
              ]
                .filter(Boolean)
                .join(' · ')}
            </dd>
          </div>
        </>
      }
    />
  )
}
