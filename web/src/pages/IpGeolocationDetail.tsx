// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useParams } from 'react-router'
import { useGoBack } from '@/hooks/useGoBack'
import { useQuery } from '@tanstack/react-query'
import { getIpGeolocationOptions } from '@/api/client/@tanstack/react-query.gen'
import type { GetIpGeolocationResponse } from '@/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { Button, Detail, PageState, type DetailFact } from '@temps-sdk/ds'
import { ArrowLeft, MapPin, Globe, AlertCircle } from 'lucide-react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

function geoFacts(geoData: GetIpGeolocationResponse): DetailFact[] {
  return [
    {
      label: 'IP address',
      value: <span className="font-mono">{geoData.ip}</span>,
    },
    { label: 'Country', value: geoData.country || 'Not available' },
    { label: 'Region', value: geoData.region || 'Not available' },
    { label: 'City', value: geoData.city || 'Not available' },
    { label: 'Timezone', value: geoData.timezone || 'Not available' },
    {
      label: 'European Union',
      value: (
        <Badge variant={geoData.is_eu ? 'default' : 'secondary'}>
          {geoData.is_eu ? 'Yes' : 'No'}
        </Badge>
      ),
    },
  ]
}

function IpGeolocationDetailSkeleton({
  backAction,
}: {
  backAction: React.ReactNode
}) {
  return (
    <Detail
      title={<Skeleton className="h-7 w-48" />}
      actions={backAction}
      facts={[0, 1, 2, 3, 4, 5].map(() => ({
        label: <Skeleton className="h-3 w-16" />,
        value: <Skeleton className="h-4 w-20" />,
      }))}
      main={<Skeleton className="h-32 w-full" />}
    />
  )
}

export default function IpGeolocationDetail() {
  const { ip } = useParams<{ ip: string }>()
  const goBack = useGoBack('/proxy-logs')
  const { setBreadcrumbs } = useBreadcrumbs()

  usePageTitle(`IP Geolocation - ${ip}`)

  const {
    data: geoData,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getIpGeolocationOptions({
      path: {
        ip: ip || '',
      },
    }),
    enabled: !!ip,
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'IP Geolocation' }])
  }, [setBreadcrumbs])

  const backAction = (
    <Button onClick={() => goBack()} variant="ghost" size="sm">
      <ArrowLeft className="mr-2 h-4 w-4" />
      Back
    </Button>
  )

  if (isLoading) {
    return <IpGeolocationDetailSkeleton backAction={backAction} />
  }

  if (error) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Failed to load geolocation data"
        description={`Unable to retrieve geolocation information for IP address ${ip}. This could be because the IP is not in our database or the IP address is invalid.`}
        action={<Button onClick={() => void refetch()}>Retry</Button>}
      />
    )
  }

  if (!geoData) {
    return (
      <PageState
        variant="empty"
        icon={Globe}
        title="No geolocation data"
        description={`No geolocation data is available for IP address ${ip}.`}
        action={backAction}
      />
    )
  }

  const hasCoordinates = geoData.latitude !== null || geoData.longitude !== null

  return (
    <Detail
      title={<span className="font-mono">{ip}</span>}
      description="Location data for this IP address"
      actions={backAction}
      facts={geoFacts(geoData)}
      main={
        hasCoordinates ? (
          <Card>
            <CardHeader className="border-b px-5 py-4">
              <CardTitle className="text-base flex items-center gap-2">
                <Globe className="h-4 w-4" />
                Coordinates
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-1 gap-6 md:grid-cols-2">
                <div className="space-y-1">
                  <h4 className="text-sm font-medium text-muted-foreground">
                    Latitude
                  </h4>
                  <p className="text-sm font-mono">
                    {geoData.latitude ?? 'Not available'}
                  </p>
                </div>
                <div className="space-y-1">
                  <h4 className="text-sm font-medium text-muted-foreground">
                    Longitude
                  </h4>
                  <p className="text-sm font-mono">
                    {geoData.longitude ?? 'Not available'}
                  </p>
                </div>
              </div>
              {geoData.latitude && geoData.longitude && (
                <div className="mt-4">
                  <a
                    href={`https://www.google.com/maps?q=${geoData.latitude},${geoData.longitude}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="flex items-center gap-1 text-sm text-primary hover:underline"
                  >
                    <MapPin className="h-3 w-3" />
                    View on Google Maps
                  </a>
                </div>
              )}
            </CardContent>
          </Card>
        ) : (
          <p className="text-sm text-muted-foreground">
            No coordinate data available for this IP address.
          </p>
        )
      }
    />
  )
}
