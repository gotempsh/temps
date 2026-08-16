/** Human-readable, non-secret description of an ambient host CLI login. */
export function hostAuthLabel(authMethod?: string | null): string {
  switch (authMethod) {
    case 'chatgpt_subscription':
      return 'Host environment · ChatGPT subscription'
    case 'api_key_env':
      return 'Host environment · OPENAI_API_KEY'
    case 'api_key_file':
      return 'Host environment · Codex API-key login'
    case 'host_auth_store':
      return 'Host environment · CLI auth store'
    case 'oauth':
      return 'Host environment · OAuth login'
    default:
      return 'Authenticated from the host environment'
  }
}
