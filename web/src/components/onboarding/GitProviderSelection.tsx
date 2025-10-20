import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  GithubIcon,
  GitBranch,
  ArrowRight,
  Code2,
  GitBranchIcon,
} from 'lucide-react'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'

export type GitProvider = 'github' | 'gitlab' | 'bitbucket' | 'gitea'

interface GitProviderSelectionProps {
  onProviderSelect: (provider: GitProvider, domain: string) => void
}

interface ProviderOption {
  id: GitProvider
  name: string
  icon: React.ElementType
  description: string
  available: boolean
  defaultDomain: string
}

const providers: ProviderOption[] = [
  {
    id: 'github',
    name: 'GitHub',
    icon: GithubIcon,
    description: 'Connect with GitHub.com or GitHub Enterprise',
    available: true,
    defaultDomain: 'github.com',
  },
  {
    id: 'gitlab',
    name: 'GitLab',
    icon: GitBranch,
    description: 'Connect with GitLab.com or self-hosted GitLab',
    available: true,
    defaultDomain: 'gitlab.com',
  },
  {
    id: 'bitbucket',
    name: 'Bitbucket',
    icon: Code2,
    description: 'Connect with Bitbucket Cloud or Server',
    available: false,
    defaultDomain: 'bitbucket.org',
  },
  {
    id: 'gitea',
    name: 'Gitea',
    icon: GitBranchIcon,
    description: 'Connect with self-hosted Gitea instance',
    available: false,
    defaultDomain: '',
  },
]

export function GitProviderSelection({
  onProviderSelect,
}: GitProviderSelectionProps) {
  const [selectedProvider, setSelectedProvider] = useState<GitProvider | null>(
    null
  )
  const [customDomain, setCustomDomain] = useState('')
  const [showCustomDomain, setShowCustomDomain] = useState(false)

  const handleProviderClick = (provider: ProviderOption) => {
    if (!provider.available) return

    setSelectedProvider(provider.id)
    if (provider.id === 'gitlab' || provider.id === 'gitea') {
      setShowCustomDomain(true)
    }
  }

  const handleContinue = () => {
    if (!selectedProvider) return

    const provider = providers.find((p) => p.id === selectedProvider)
    if (!provider) return

    const domain = customDomain || provider.defaultDomain
    onProviderSelect(selectedProvider, domain)
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold">Connect Your Git Provider</h2>
        <p className="text-muted-foreground mt-2">
          Choose your Git provider to enable automatic deployments
        </p>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {providers.map((provider) => {
          const Icon = provider.icon
          return (
            <button
              key={provider.id}
              onClick={() => handleProviderClick(provider)}
              disabled={!provider.available}
              className={`relative p-6 border-2 rounded-lg transition-all ${
                !provider.available
                  ? 'border-border opacity-60 cursor-not-allowed'
                  : selectedProvider === provider.id
                    ? 'border-primary ring-2 ring-primary/20 bg-primary/5'
                    : 'border-border hover:border-muted-foreground/50 hover:bg-accent cursor-pointer'
              }`}
            >
              {!provider.available && (
                <Badge
                  variant="secondary"
                  className="absolute top-3 right-3 text-xs"
                >
                  Coming Soon
                </Badge>
              )}
              <div className="flex flex-col items-center space-y-3">
                <Icon className="h-12 w-12" />
                <div className="text-center">
                  <h3 className="font-semibold">{provider.name}</h3>
                  <p className="text-sm text-muted-foreground mt-1">
                    {provider.description}
                  </p>
                </div>
              </div>
            </button>
          )
        })}
      </div>

      {showCustomDomain &&
        (selectedProvider === 'gitlab' || selectedProvider === 'gitea') && (
          <div className="space-y-4 p-4 border rounded-lg bg-accent/50">
            <div className="space-y-2">
              <Label htmlFor="domain">
                {selectedProvider === 'gitlab' ? 'GitLab' : 'Gitea'} Domain
              </Label>
              <Input
                id="domain"
                type="text"
                placeholder={
                  selectedProvider === 'gitlab'
                    ? 'gitlab.com or your-gitlab.example.com'
                    : 'your-gitea.example.com'
                }
                value={customDomain}
                onChange={(e) => setCustomDomain(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                Enter your {selectedProvider === 'gitlab' ? 'GitLab' : 'Gitea'}{' '}
                instance domain
                {selectedProvider === 'gitlab' &&
                  ' (leave empty for gitlab.com)'}
              </p>
            </div>
          </div>
        )}

      {selectedProvider && (
        <div className="flex justify-end">
          <Button
            onClick={handleContinue}
            disabled={selectedProvider === 'gitea' && !customDomain}
          >
            Continue
            <ArrowRight className="ml-2 h-4 w-4" />
          </Button>
        </div>
      )}
    </div>
  )
}
