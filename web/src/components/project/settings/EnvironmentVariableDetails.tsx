// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { EnvironmentVariableResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { CredentialDetails } from './CredentialDetails'
import { EnvironmentVariableRevealValue } from './EnvironmentVariableRevealValue'

export function EnvironmentVariableDetails({
  projectId,
  variable,
  detailPath,
}: {
  projectId: number
  variable: EnvironmentVariableResponse
  detailPath: string
}) {
  return (
    <CredentialDetails
      projectId={projectId}
      subject={{ kind: 'env_var', id: variable.id, key: variable.key }}
      detailPath={detailPath}
      createdAt={variable.created_at}
      badge={
        <Badge variant="secondary">
          {variable.is_secret ? 'Secret' : 'Regular'}
        </Badge>
      }
      facts={
        <>
          <div>
            <dt className="text-muted-foreground">Value</dt>
            <EnvironmentVariableRevealValue
              projectId={projectId}
              variableId={variable.id}
              name={variable.key}
              isSecret={variable.is_secret ?? false}
              version={String(variable.updated_at)}
            />
          </div>
          <div>
            <dt className="text-muted-foreground">Environments</dt>
            <dd className="mt-1 flex flex-wrap gap-1">
              {variable.environments.length
                ? variable.environments.map((env) => (
                    <Badge key={env.id} variant="outline">
                      {env.name}
                    </Badge>
                  ))
                : 'None'}
            </dd>
          </div>
        </>
      }
    />
  )
}
