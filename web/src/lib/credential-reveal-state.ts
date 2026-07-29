export interface ScopedCredentialValue {
  value: string
  scope: string
}

export function credentialValueForScope(
  credential: ScopedCredentialValue | undefined,
  currentScope: string
): string | undefined {
  return credential?.scope === currentScope ? credential.value : undefined
}
