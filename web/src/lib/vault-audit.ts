// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// These identifiers match the operations persisted by the Vault handlers and authority.
export const VAULT_AUDIT_OPERATIONS = [
  {
    value: 'SANDBOX_SERVICE_ACCESS_REQUESTED',
    label: 'Sandbox service requested',
  },
  {
    value: 'SANDBOX_SERVICE_ACCESS_APPROVED',
    label: 'Sandbox service approved',
  },
  { value: 'SANDBOX_SERVICE_ACCESS_REVOKED', label: 'Sandbox service revoked' },
  { value: 'CREATE_ACCESS_VAULT', label: 'Vault created' },
  { value: 'UPDATE_ACCESS_VAULT', label: 'Vault updated' },
  { value: 'CREATE_ACCESS_BUNDLE', label: 'Access bundle created' },
  { value: 'UPDATE_ACCESS_BUNDLE', label: 'Access bundle updated' },
  { value: 'CREATE_ACCESS_SESSION', label: 'Vault session issued' },
  { value: 'REVOKE_ACCESS_SESSION', label: 'Vault session revoked' },
  { value: 'SANDBOX_ACCESS_SERVICE_CREATED', label: 'Vault service created' },
  { value: 'SANDBOX_ACCESS_SERVICE_UPDATED', label: 'Vault service updated' },
  {
    value: 'SANDBOX_SERVICE_GRANT_REQUESTED',
    label: 'Service access requested',
  },
  { value: 'SANDBOX_SERVICE_GRANT_APPROVED', label: 'Service access approved' },
  { value: 'SANDBOX_SERVICE_GRANT_REVOKED', label: 'Service access revoked' },
  {
    value: 'CREATE_SANDBOX_ACCESS_AUTHORITY',
    label: 'Worker access authority registered',
  },
  {
    value: 'CLOSE_SANDBOX_ACCESS_AUTHORITY',
    label: 'Worker access authority closed',
  },
  {
    value: 'RETIRE_SANDBOX_ACCESS_AUTHORIZATION',
    label: 'Sandbox access authorization retired',
  },
] as const

export function vaultAuditLabel(operation: string): string | undefined {
  return VAULT_AUDIT_OPERATIONS.find((entry) => entry.value === operation)
    ?.label
}
