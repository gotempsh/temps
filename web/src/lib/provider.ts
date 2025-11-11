import { ProviderResponse } from '@/api/client/types.gen'

// Helper function to check if provider is GitHub App
export const isGitHubApp = (provider: ProviderResponse) =>
  provider.provider_type === 'github' &&
  (provider.auth_method === 'app' || provider.auth_method === 'github_app')

// Helper function to check if provider is GitLab OAuth
export const isGitLabOAuth = (provider: ProviderResponse) =>
  provider.provider_type === 'gitlab' && provider.auth_method === 'oauth'
