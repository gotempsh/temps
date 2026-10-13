// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Environment variables and secrets reach an app when its container starts.
 * Saving one changes nothing in a running deployment until the next deploy,
 * so the console says that at the moment of the change and offers to redeploy
 * the affected environments, reusing the service-link redeploy routing (each
 * deployment rebuilt from its own commit, image or bundle).
 */

import type { EnvironmentResponse } from '@/api/client'
import { environmentsToRedeploy } from './service-link-redeploy'

/**
 * Environments a change touched: explicit ids, or `'all'` for a secret that
 * is not scoped (an unscoped secret is mounted everywhere).
 */
export type VariableChangeScope = readonly number[] | 'all'

/** Running, non-preview environments the change applies to. */
export function environmentsAffectedByChange(
  environments: EnvironmentResponse[] | undefined,
  scope: VariableChangeScope
): EnvironmentResponse[] {
  return environmentsToRedeploy(environments).filter(
    (environment) => scope === 'all' || scope.includes(environment.id)
  )
}

/** Union of the environment ids of several changed items. */
export function combinedScope(
  scopes: readonly VariableChangeScope[]
): VariableChangeScope {
  if (scopes.includes('all')) return 'all'
  return [...new Set((scopes as readonly (readonly number[])[]).flat())]
}

/** Toast body after a change, e.g. "Applies on next deploy. Redeploy production to apply it now." */
export function variableChangeRedeployMessage(
  targets: Pick<EnvironmentResponse, 'name'>[]
): string {
  const names = targets.map((environment) => environment.name)
  const where =
    names.length === 1
      ? ` Redeploy ${names[0]} to apply it now.`
      : names.length > 1
        ? ` Redeploy ${names.length} environments (${names.join(', ')}) to apply it now.`
        : ''
  return `Applies on next deploy.${where}`
}
