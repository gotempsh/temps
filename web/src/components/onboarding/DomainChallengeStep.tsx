import { useState, useEffect } from 'react'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { CheckCircle2, Loader2, Shield, Info } from 'lucide-react'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  createDomainMutation,
  listDomainsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'
import { DomainResponse } from '@/api/client/types.gen'
import { DomainChallengeView } from '@/components/domain/DomainChallengeView'

type ChallengeType = 'http-01' | 'dns-01'

interface DomainChallengeStepProps {
  baseDomain: string
  onSuccess: (domain: DomainResponse) => void
  onBack: () => void
}

export function DomainChallengeStep({
  baseDomain,
  onSuccess,
  onBack,
}: DomainChallengeStepProps) {
  // For wildcard domains, DNS-01 is the only valid challenge type
  const challengeType: ChallengeType = 'dns-01'
  const [createdDomain, setCreatedDomain] = useState<DomainResponse | null>(
    null
  )
  const [showDnsInstructions, setShowDnsInstructions] = useState(false)

  const wildcardDomain = `*.${baseDomain}`

  // Check if domain already exists
  const { data: domains, isLoading: domainsLoading } = useQuery({
    ...listDomainsOptions({}),
    retry: false,
  })

  // Check if wildcard domain exists
  useEffect(() => {
    if (domains?.domains) {
      const existingDomain = domains.domains.find(
        (d: any) => d.domain === wildcardDomain
      )
      if (existingDomain) {
        // Use queueMicrotask to defer state updates and avoid cascading renders
        queueMicrotask(() => {
          setCreatedDomain(existingDomain)
          // If it has DNS challenge info, show that screen
          if (existingDomain.dns_challenge_token) {
            setShowDnsInstructions(true)
          }
        })
      }
    }
  }, [domains, wildcardDomain])

  const createDomain = useMutation({
    ...createDomainMutation(),
    meta: {
      errorTitle: 'Failed to create domain',
    },
    onSuccess: (data) => {
      setCreatedDomain(data)

      // For DNS-01 challenges (wildcard domains), always show DNS instructions
      if (challengeType === 'dns-01') {
        setShowDnsInstructions(true)
        toast.success('Domain created! Please complete DNS challenge.')
      } else {
        toast.success(
          'Domain added successfully! Certificate provisioning has started.'
        )
        onSuccess(data)
      }
    },
  })

  const handleCreateDomain = () => {
    createDomain.mutate({
      body: {
        domain: wildcardDomain,
        challenge_type: challengeType,
      },
    })
  }

  // If DNS instructions are shown, use the reusable DomainChallengeView component
  if (showDnsInstructions && createdDomain) {
    return (
      <DomainChallengeView
        domain={createdDomain}
        onSuccess={onSuccess}
        onBack={onBack}
        showBackButton={true}
        showContinueButton={true}
      />
    )
  }

  // If domain already exists and is valid, show success message
  if (
    createdDomain &&
    !showDnsInstructions &&
    createdDomain.status !== 'pending'
  ) {
    return (
      <div className="space-y-6">
        <div className="text-center space-y-2">
          <h2 className="text-2xl font-bold">Domain Already Configured</h2>
          <p className="text-muted-foreground">
            Your wildcard domain is already set up
          </p>
        </div>

        <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
          <CheckCircle2 className="h-4 w-4 text-green-600" />
          <AlertDescription>
            <p className="text-sm mb-2">
              <strong>Domain found:</strong>{' '}
              <code className="font-mono text-xs bg-muted px-1.5 py-0.5 rounded">
                {wildcardDomain}
              </code>
            </p>
            <p className="text-sm">
              Status: <strong>{createdDomain.status}</strong>
            </p>
          </AlertDescription>
        </Alert>

        <div className="flex justify-between">
          <Button variant="outline" onClick={onBack}>
            Back
          </Button>
          <Button onClick={() => onSuccess(createdDomain)}>Continue</Button>
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Domain Challenge Verification</h2>
        <p className="text-muted-foreground">
          Choose how to verify domain ownership for SSL certificates
        </p>
      </div>

      {domainsLoading && (
        <Alert>
          <Loader2 className="h-4 w-4 animate-spin" />
          <AlertDescription>Checking for existing domains...</AlertDescription>
        </Alert>
      )}

      <Alert>
        <Info className="h-4 w-4" />
        <AlertDescription>
          We&apos;ll create a wildcard domain:{' '}
          <code className="font-mono text-xs bg-muted px-1.5 py-0.5 rounded">
            {wildcardDomain}
          </code>
          <br />
          This will allow all your projects to use subdomains automatically.
        </AlertDescription>
      </Alert>

      {/* Wildcard domains require DNS-01 challenge */}
      <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
        <Shield className="h-4 w-4 text-blue-600" />
        <AlertTitle>DNS-01 Challenge Required</AlertTitle>
        <AlertDescription>
          <p className="mb-3">
            Wildcard SSL certificates require DNS-01 challenge verification.
            After clicking continue, you&apos;ll receive DNS TXT record values
            that you need to add to your DNS provider.
          </p>
          <div className="space-y-1">
            <div className="flex items-center gap-2 text-xs">
              <CheckCircle2 className="h-3 w-3 text-green-600" />
              <span>Works behind firewalls</span>
            </div>
            <div className="flex items-center gap-2 text-xs">
              <CheckCircle2 className="h-3 w-3 text-green-600" />
              <span>Supports wildcard certificates</span>
            </div>
            <div className="flex items-center gap-2 text-xs">
              <Info className="h-3 w-3 text-blue-600" />
              <span>Requires manual DNS TXT record setup</span>
            </div>
          </div>
        </AlertDescription>
      </Alert>

      <div className="flex justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={handleCreateDomain} disabled={createDomain.isPending}>
          {createDomain.isPending ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              Creating Domain...
            </>
          ) : (
            'Create Domain & Verify'
          )}
        </Button>
      </div>
    </div>
  )
}
