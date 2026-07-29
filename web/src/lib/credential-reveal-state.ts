export interface ScopedCredentialValue {
  value: string
  scope: string
}

export interface CredentialRevealGuard {
  begin(field: string): symbol
  isCurrent(field: string, request: symbol): boolean
  finish(field: string, request: symbol): boolean
  cancel(field: string): void
  invalidate(): void
}

export function createCredentialRevealGuard(): CredentialRevealGuard {
  let active = true
  const requests = new Map<string, symbol>()

  return {
    begin(field) {
      const request = Symbol(field)
      requests.set(field, request)
      return request
    },
    isCurrent(field, request) {
      return active && requests.get(field) === request
    },
    finish(field, request) {
      if (!active || requests.get(field) !== request) return false
      requests.delete(field)
      return true
    },
    cancel(field) {
      requests.delete(field)
    },
    invalidate() {
      active = false
      requests.clear()
    },
  }
}

export function credentialValueForScope(
  credential: ScopedCredentialValue | undefined,
  currentScope: string
): string | undefined {
  return credential?.scope === currentScope ? credential.value : undefined
}
