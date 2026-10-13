// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { deliveryError } from '@/components/domains/delivery-errors'
import {
  deliveryProfileQueryKey,
  fetchDeliveryProfile,
} from '@/components/domains/delivery-queries'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Detail, type DetailFact } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useEffect } from 'react'
import { Link, useParams } from 'react-router'

/** Shown in place of Bunny account details the API omits for this caller. */
const PROVIDER_DETAIL_HIDDEN = 'Visible with DNS provider read access'

export default function DeliveryProfileDetail() {
  const { id } = useParams<{ id: string }>()
  const profileId = Number(id)
  const validId = Number.isSafeInteger(profileId) && profileId > 0
  const { setBreadcrumbs } = useBreadcrumbs()
  const profileQuery = useQuery({
    queryKey: deliveryProfileQueryKey(profileId),
    // A missing profile resolves to null so the page can say it does not
    // exist, rather than retrying the 404 and reporting a failure.
    queryFn: () => fetchDeliveryProfile(profileId),
    enabled: validId,
  })
  const profile = profileQuery.data ?? undefined

  usePageTitle(profile?.name ?? 'Delivery profile')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Delivery profiles', href: '/delivery-profiles' },
      { label: profile?.name ?? 'Delivery profile' },
    ])
  }, [profile?.name, setBreadcrumbs])

  const back = (
    <Button variant="outline" asChild>
      <Link to="/delivery-profiles">
        <ArrowLeft className="mr-2 size-4" />
        Back to profiles
      </Link>
    </Button>
  )

  if (validId && profileQuery.isPending) {
    return (
      <PageContainer>
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-36 w-full" />
      </PageContainer>
    )
  }
  if (profileQuery.isError) {
    return (
      <PageContainer>
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(profileQuery.error)}{' '}
            <Button variant="link" onClick={() => profileQuery.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
        {back}
      </PageContainer>
    )
  }
  if (!profile) {
    return (
      <PageContainer>
        <PageHeader
          title="Delivery profile not found"
          description={
            validId
              ? `No delivery profile has ID ${profileId}. It may have been deleted.`
              : `"${id ?? ''}" is not a delivery profile ID.`
          }
          actions={back}
        />
      </PageContainer>
    )
  }

  const delivery =
    profile.provider_kind === 'cloudflare'
      ? 'Cloudflare proxy'
      : profile.provider_kind === 'bunny'
        ? 'bunny.net CDN'
        : 'Direct to Temps origin'
  const facts: DetailFact[] = [
    { label: 'Delivery', value: delivery },
    {
      label: 'Created',
      value: new Date(profile.created_at).toLocaleDateString(),
    },
    { label: 'Profile ID', value: profile.id },
  ]

  return (
    <Detail
      title={profile.name}
      description="A reusable delivery choice for project defaults, environments, and domains. Creating a profile does not change existing domains."
      actions={back}
      facts={facts}
      main={
        <Card>
          <CardHeader>
            <CardTitle>Configuration</CardTitle>
            <CardDescription>
              How this profile delivers traffic to your applications
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-5 text-sm">
            {profile.provider_kind === 'bunny' ? (
              <>
                <dl className="grid gap-4 sm:grid-cols-2">
                  <div>
                    <dt className="font-medium">Pull Zone ID</dt>
                    <dd className="mt-1 text-muted-foreground">
                      {profile.bunny_pull_zone_id ?? PROVIDER_DETAIL_HIDDEN}
                    </dd>
                  </div>
                  <div>
                    <dt className="font-medium">Pull Zone hostname</dt>
                    <dd className="mt-1 break-all text-muted-foreground">
                      {profile.bunny_hostname ?? PROVIDER_DETAIL_HIDDEN}
                    </dd>
                  </div>
                  <div>
                    <dt className="font-medium">API key</dt>
                    <dd className="mt-1 text-muted-foreground">
                      Stored securely; it cannot be viewed again
                    </dd>
                  </div>
                </dl>
                <p className="text-muted-foreground">
                  The Pull Zone and API key were verified when this profile was
                  created. Create a new profile to use another Pull Zone or key.
                </p>
              </>
            ) : (
              <>
                <dl>
                  <dt className="font-medium">DNS connection</dt>
                  <dd className="mt-1 text-muted-foreground">
                    Selected for each domain during domain setup
                  </dd>
                </dl>
                <p className="text-muted-foreground">
                  {profile.provider_kind === 'cloudflare'
                    ? 'Cloudflare credentials and zones are managed in DNS providers. Other Cloudflare profiles behave the same way; their names and assignments are the only differences.'
                    : 'Direct delivery has no profile-specific credentials. Choose the DNS connection and origin when setting up a domain.'}
                </p>
                {profile.provider_kind === 'cloudflare' && (
                  <Button variant="outline" asChild>
                    <Link to="/dns-providers">Manage DNS providers</Link>
                  </Button>
                )}
              </>
            )}
          </CardContent>
        </Card>
      }
    />
  )
}
