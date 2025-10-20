import { useEffect } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { getIpGeolocationOptions } from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { ArrowLeft, MapPin, Globe, AlertCircle } from 'lucide-react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function IpGeolocationDetail() {
  const { ip } = useParams<{ ip: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  usePageTitle(`IP Geolocation - ${ip}`)

  const {
    data: geoData,
    isLoading,
    error,
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

  const handleBack = () => {
    navigate(-1)
  }

  if (error) {
    return (
      <div className="container max-w-5xl mx-auto py-6">
        <div className="flex items-center gap-4 mb-4">
          <Button onClick={handleBack} variant="ghost" size="sm">
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back
          </Button>
        </div>
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Failed to load geolocation data</AlertTitle>
          <AlertDescription>
            Unable to retrieve geolocation information for IP address {ip}. This
            could be because the IP is not in our database or the IP address is
            invalid.
          </AlertDescription>
        </Alert>
      </div>
    )
  }

  return (
    <div className="container max-w-5xl mx-auto py-6 space-y-4">
      <div className="flex items-center gap-4">
        <Button onClick={handleBack} variant="ghost" size="sm">
          <ArrowLeft className="h-4 w-4 mr-2" />
          Back
        </Button>
      </div>

      <Card>
        <CardHeader>
          <div className="flex items-center gap-3">
            <Globe className="h-6 w-6" />
            <div>
              <CardTitle>IP Geolocation Information</CardTitle>
              <p className="text-sm text-muted-foreground mt-1">
                Location data for IP address:{' '}
                <span className="font-mono">{ip}</span>
              </p>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="space-y-4">
              <Skeleton className="h-32 w-full" />
              <Skeleton className="h-32 w-full" />
              <Skeleton className="h-32 w-full" />
            </div>
          ) : geoData ? (
            <div className="space-y-6">
              {/* IP Address Information */}
              <Card>
                <CardHeader>
                  <CardTitle className="text-base flex items-center gap-2">
                    <MapPin className="h-4 w-4" />
                    IP Address Details
                  </CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        IP Address
                      </h4>
                      <p className="text-sm font-mono">{geoData.ip}</p>
                    </div>
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        European Union
                      </h4>
                      <Badge variant={geoData.is_eu ? 'default' : 'secondary'}>
                        {geoData.is_eu ? 'Yes' : 'No'}
                      </Badge>
                    </div>
                  </div>
                </CardContent>
              </Card>

              {/* Location Information */}
              <Card>
                <CardHeader>
                  <CardTitle className="text-base flex items-center gap-2">
                    <Globe className="h-4 w-4" />
                    Geographic Location
                  </CardTitle>
                </CardHeader>
                <CardContent>
                  <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Country
                      </h4>
                      <p className="text-sm">
                        {geoData.country || 'Not available'}
                      </p>
                    </div>
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Country Code
                      </h4>
                      <p className="text-sm font-mono">
                        {geoData.country_code || 'Not available'}
                      </p>
                    </div>
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Region
                      </h4>
                      <p className="text-sm">
                        {geoData.region || 'Not available'}
                      </p>
                    </div>
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        City
                      </h4>
                      <p className="text-sm">
                        {geoData.city || 'Not available'}
                      </p>
                    </div>
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Timezone
                      </h4>
                      <p className="text-sm">
                        {geoData.timezone || 'Not available'}
                      </p>
                    </div>
                  </div>
                </CardContent>
              </Card>

              {/* Coordinates */}
              {(geoData.latitude !== null || geoData.longitude !== null) && (
                <Card>
                  <CardHeader>
                    <CardTitle className="text-base flex items-center gap-2">
                      <Globe className="h-4 w-4" />
                      Coordinates
                    </CardTitle>
                  </CardHeader>
                  <CardContent>
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
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
                          className="text-sm text-primary hover:underline flex items-center gap-1"
                        >
                          <MapPin className="h-3 w-3" />
                          View on Google Maps
                        </a>
                      </div>
                    )}
                  </CardContent>
                </Card>
              )}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              No geolocation data available
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
