// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  GitBranch,
  GitPullRequest,
  LockKeyhole,
} from 'lucide-react'
import { Link } from 'react-router'
import {
  Button,
  Field,
  GitProviderMark,
  PageContainer,
  Status,
  Wizard,
  cn,
  useUrlState,
} from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'

const STEPS = [
  {
    id: 'provider',
    label: 'Choose provider',
    description: 'Where your code lives',
  },
  {
    id: 'repository',
    label: 'Select repository',
    description: 'Repository and branch',
  },
  { id: 'done', label: 'Connection ready', description: 'Review the result' },
]
const PROVIDERS = [
  { name: 'GitHub', description: 'Personal and organization repositories' },
  { name: 'GitLab', description: 'Projects in your account or group' },
  { name: 'Bitbucket', description: 'Repositories in your workspace' },
]
const REPOSITORY_PATTERN = /^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/

/** Sample-only flow: URL state reproduces the selection; no provider is contacted. */
export default function ConnectRepoWizard() {
  const { get, patch } = useUrlState<'step' | 'provider' | 'repository'>()
  const provider = PROVIDERS.find((item) => item.name === get('provider'))
  const repo = get('repository') ?? ''
  const validRepository = REPOSITORY_PATTERN.test(repo.trim())
  const requestedStep = get('step') ?? 'provider'
  const step = !provider
    ? 'provider'
    : requestedStep === 'done' && validRepository
      ? 'done'
      : requestedStep === 'repository' || requestedStep === 'done'
        ? 'repository'
        : 'provider'
  const [error, setError] = useState<string>()

  const connect = () => {
    if (!validRepository) {
      setError(
        'Enter the owner and repository name, for example sample-team/checkout-api.',
      )
      return
    }
    setError(undefined)
    patch({ repository: repo.trim(), step: 'done' })
  }

  const footer =
    step === 'provider' ? (
      <>
        <p className="text-sm text-muted-foreground">
          {provider
            ? `${provider.name} selected. Continue to choose a repository.`
            : 'Choose a provider to continue.'}
        </p>
        <Button
          aria-disabled={!provider}
          className={cn(
            'w-full sm:w-auto',
            !provider && 'opacity-50 cursor-not-allowed',
          )}
          onClick={() => {
            if (provider) patch({ step: 'repository' })
          }}
        >
          Continue <ArrowRight className="size-4" />
        </Button>
      </>
    ) : step === 'repository' ? (
      <>
        <Button
          variant="ghost"
          onClick={() => {
            setError(undefined)
            patch({ step: 'provider' })
          }}
        >
          <ArrowLeft className="size-4" /> Back
        </Button>
        <Button type="submit" form="connect-sample-repository">
          Connect sample repository <ArrowRight className="size-4" />
        </Button>
      </>
    ) : (
      <>
        <Button
          variant="outline"
          onClick={() => {
            setError(undefined)
            patch({ step: null, provider: null, repository: null })
          }}
        >
          Start again
        </Button>
        <Button asChild>
          <Link to="/detail">
            View sample deployment <ArrowRight className="size-4" />
          </Link>
        </Button>
      </>
    )

  return (
    <PageContainer>
      <Wizard
        fullWidth
        title="Connect a repository"
        description="Choose your Git provider and the repository you want to deploy."
        currentStep={step}
        steps={STEPS}
        headerActions={
          <span className="rounded-md border px-2 py-1 text-xs text-muted-foreground">
            Interactive example
          </span>
        }
        footer={footer}
      >
        {step === 'provider' && (
          <div className="grid gap-8 lg:grid-cols-3">
            <fieldset className="min-w-0 space-y-5 lg:col-span-2">
              <legend className="text-lg font-semibold">
                Where is your repository hosted?
              </legend>
              <p className="text-sm text-muted-foreground">
                Select the provider that holds the code you want to deploy.
              </p>
              <div className="space-y-3">
                {PROVIDERS.map((item) => {
                  const selected = provider?.name === item.name
                  return (
                    <Button
                      key={item.name}
                      variant="outline"
                      aria-pressed={selected}
                      onClick={() => patch({ provider: item.name })}
                      className={cn(
                        'h-auto w-full justify-start gap-4 whitespace-normal p-4 text-left',
                        selected &&
                          'border-primary bg-accent text-accent-foreground ring-1 ring-primary',
                      )}
                    >
                      <span className="flex size-10 shrink-0 items-center justify-center rounded-md border bg-background">
                        <GitProviderMark provider={item.name} />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium">{item.name}</span>
                        <span className="mt-1 block text-sm font-normal text-muted-foreground">
                          {item.description}
                        </span>
                      </span>
                      <span
                        className={cn(
                          'flex size-5 shrink-0 items-center justify-center rounded-full border',
                          selected
                            ? 'border-primary bg-primary text-primary-foreground'
                            : 'border-border',
                        )}
                      >
                        {selected && <Check className="size-3" />}
                      </span>
                    </Button>
                  )
                })}
              </div>
            </fieldset>
            <aside className="space-y-5 border-t pt-6 lg:border-l lg:border-t-0 lg:pl-6 lg:pt-0">
              <h2 className="text-sm font-semibold">Before you connect</h2>
              <div className="flex gap-3">
                <LockKeyhole className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <p className="text-sm text-muted-foreground">
                  In the console, use an account with access to the repository
                  you want to deploy.
                </p>
              </div>
              <div className="flex gap-3">
                <GitPullRequest className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <p className="text-sm text-muted-foreground">
                  Next, choose a repository and review its deployment branch.
                </p>
              </div>
              <p className="border-t pt-4 text-xs text-muted-foreground">
                This example uses sample data. It does not sign in to a provider
                or change any repositories.
              </p>
            </aside>
          </div>
        )}

        {step === 'repository' && (
          <form
            id="connect-sample-repository"
            onSubmit={(event) => {
              event.preventDefault()
              connect()
            }}
            className="grid gap-8 lg:grid-cols-3"
          >
            <div className="min-w-0 space-y-6 lg:col-span-2">
              <div>
                <h2 className="text-lg font-semibold">Select a repository</h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  Enter a repository path from {provider?.name} to preview the
                  connection.
                </p>
              </div>
              <Field
                label="Repository"
                description="Use the owner/repository format, without a URL."
                error={error}
              >
                {(props) => (
                  <Input
                    {...props}
                    autoFocus
                    autoComplete="off"
                    value={repo}
                    placeholder="sample-team/checkout-api"
                    onChange={(event) => {
                      setError(undefined)
                      patch({ repository: event.target.value || null })
                    }}
                  />
                )}
              </Field>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => {
                  setError(undefined)
                  patch({ repository: 'sample-team/checkout-api' })
                }}
              >
                Use sample repository
              </Button>
              <div className="space-y-2">
                <p className="text-sm font-medium">Deployment branch</p>
                <div className="flex items-center gap-2 text-sm">
                  <GitBranch className="size-4 text-muted-foreground" />
                  <code>main</code>
                  <span className="text-muted-foreground">
                    · default in this example
                  </span>
                </div>
              </div>
            </div>
            <aside className="space-y-3 border-t pt-6 lg:border-l lg:border-t-0 lg:pl-6 lg:pt-0">
              <h2 className="text-sm font-semibold">Connection summary</h2>
              <dl className="space-y-3 text-sm">
                <div>
                  <dt className="text-muted-foreground">Git provider</dt>
                  <dd className="mt-1 font-medium">{provider?.name}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Access</dt>
                  <dd className="mt-1">
                    Sample only — no authorization required
                  </dd>
                </div>
              </dl>
              <p className="border-t pt-3 text-xs text-muted-foreground">
                Connecting here only advances the example. No webhook or
                deployment will be created.
              </p>
            </aside>
          </form>
        )}

        {step === 'done' && (
          <div className="space-y-6">
            <Status tone="ok" label="Sample connection ready" />
            <div>
              <h2 className="text-lg font-semibold break-all">{repo}</h2>
              <p className="mt-2 text-sm text-muted-foreground">
                You’ve completed the example. No repository was connected and no
                deployment was started.
              </p>
            </div>
            <dl className="grid gap-4 border-t pt-5 sm:grid-cols-3">
              <div>
                <dt className="text-sm text-muted-foreground">Provider</dt>
                <dd className="mt-1 text-sm font-medium">{provider?.name}</dd>
              </div>
              <div>
                <dt className="text-sm text-muted-foreground">Branch</dt>
                <dd className="mt-1 font-mono text-sm">main</dd>
              </div>
              <div>
                <dt className="text-sm text-muted-foreground">Next step</dt>
                <dd className="mt-1 text-sm">Explore a sample deployment</dd>
              </div>
            </dl>
          </div>
        )}
      </Wizard>
    </PageContainer>
  )
}
