import {
  cancelDomainOrderMutation,
  createOrRecreateOrderMutation,
  finalizeOrderMutation,
  getDomainOrderOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { DomainResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle,
  CheckCircle2,
  Info,
  Loader2,
  RefreshCw,
  Shield,
  XCircle,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { AcmeOrderInfo } from './AcmeOrderInfo'
import { DnsTxtRecordsDisplay } from './DnsTxtRecordsDisplay'

interface ChallengeStatus {
  type: string
  url: string
  status: string
  validated?: string
  error?: {
    type: string
    detail: string
    status: number
  }
  token: string
}

interface DomainChallengeViewProps {
  domain: DomainResponse
  onSuccess?: (domain: DomainResponse) => void
  onBack?: () => void
  showBackButton?: boolean
  showContinueButton?: boolean
}

export function DomainChallengeView({
  domain,
  onSuccess,
  onBack,
  showBackButton = false,
  showContinueButton = true,
}: DomainChallengeViewProps) {
  const [recordValidations, setRecordValidations] = useState<
    Map<number, ChallengeStatus | null>
  >(new Map())
  const [fetchingRecords, setFetchingRecords] = useState<Set<number>>(new Set())

  const wildcardDomain = domain.domain

  // Query ACME order information
  const { data: order, refetch: refetchOrder } = useQuery({
    ...getDomainOrderOptions({
      path: {
        domain_id: domain.id,
      },
    }),
    enabled: domain.status !== 'active',
    retry: false,
  })

  const createOrder = useMutation({
    ...createOrRecreateOrderMutation(),
    meta: {
      errorTitle: 'Failed to create ACME order',
    },
    onSuccess: () => {
      toast.success('ACME order created successfully')
      refetchOrder()
    },
  })

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    meta: {
      errorTitle: 'Failed to verify DNS challenge',
    },
    onSuccess: async () => {
      toast.success(
        'DNS challenge verified! SSL certificate provisioning in progress.'
      )
      // Continue to next step after successful verification
      if (onSuccess) {
        onSuccess(domain)
      }
    },
    onSettled: () => {
      // Refetch order to get updated error status
      refetchOrder()
    },
  })

  const cancelOrder = useMutation({
    ...cancelDomainOrderMutation(),
    meta: {
      errorTitle: 'Failed to cancel ACME order',
    },
    onSuccess: async () => {
      toast.success('ACME order cancelled. Creating a new order...')
      // Refetch order (should be null now)
      await refetchOrder()
    },
  })

  const handleCreateOrder = async () => {
    await createOrder.mutateAsync({
      path: {
        domain_id: domain.id,
      },
    })
  }

  const handleCompleteDns = async () => {
    await finalizeOrder.mutateAsync({
      path: {
        domain_id: domain.id,
      },
    })
  }

  const handleCancelAndRecreate = async () => {
    // Reset validation status
    setRecordValidations(new Map())

    // Cancel existing order
    await cancelOrder.mutateAsync({
      path: {
        domain_id: domain.id,
      },
    })
    // Create new order
    await createOrder.mutateAsync({
      path: {
        domain_id: domain.id,
      },
    })
  }

  const fetchValidationStatus = useCallback(
    async (validationUrl: string, recordIndex: number) => {
      setFetchingRecords((prev) => new Set(prev).add(recordIndex))
      try {
        const response = await fetch(validationUrl, {
          method: 'GET',
          headers: {
            Accept: 'application/json',
          },
        })
        const data = await response.json()
        setRecordValidations((prev) => new Map(prev).set(recordIndex, data))
        return data
      } catch (error) {
        console.error('Failed to fetch validation status:', error)
        return null
      } finally {
        setFetchingRecords((prev) => {
          const newSet = new Set(prev)
          newSet.delete(recordIndex)
          return newSet
        })
      }
    },
    []
  )

  // Get challenge info from order (DNS-01 specific)
  const challengeData = order?.authorizations as
    | {
        challenge_type: 'dns-01' | 'http-01'
        dns_txt_records: Array<{
          name: string
          value: string
          validation_url?: string
        }>
        key_authorization: string
        token: string
        validation_url?: string
      }
    | undefined

  // Auto-fetch validation status when order is available
  useEffect(() => {
    if (order && challengeData?.dns_txt_records) {
      challengeData.dns_txt_records.forEach((record, index) => {
        const validationUrl =
          record.validation_url || challengeData.validation_url
        if (
          validationUrl &&
          !recordValidations.has(index) &&
          !fetchingRecords.has(index)
        ) {
          fetchValidationStatus(validationUrl, index)
        }
      })
    }
  }, [
    order,
    challengeData,
    fetchValidationStatus,
    fetchingRecords,
    recordValidations,
  ])

  const dnsTxtRecords = challengeData?.dns_txt_records || []
  const hasDnsValues = dnsTxtRecords.length > 0

  // Check if any record has invalid status
  const hasAnyInvalidRecord = Array.from(recordValidations.values()).some(
    (status) => status?.status === 'invalid'
  )
  const hasAnyError =
    hasAnyInvalidRecord ||
    !!order?.error ||
    order?.status === 'invalid' ||
    false

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="text-center space-y-2">
        <div className="flex items-center justify-center gap-3">
          <h2 className="text-2xl font-bold">{wildcardDomain}</h2>
          <Badge variant={domain.status === 'active' ? 'default' : 'secondary'}>
            {domain.status}
          </Badge>
        </div>
        <p className="text-muted-foreground">
          SSL Certificate & Domain Verification
        </p>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Main Content */}
        <div className="lg:col-span-2 space-y-6">
          {/* DNS Challenge Instructions */}
          {(domain.status === 'challenge_requested' ||
            domain.status === 'pending_dns' ||
            domain.status === 'pending') &&
            domain.verification_method === 'dns-01' && (
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <h3 className="text-lg font-semibold">
                    DNS Challenge Required
                  </h3>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => refetchOrder()}
                  >
                    <RefreshCw className="mr-2 h-4 w-4" />
                    Refresh
                  </Button>
                </div>

                {!order && (
                  <Alert>
                    <Info className="h-4 w-4" />
                    <AlertTitle>Create ACME Order</AlertTitle>
                    <AlertDescription>
                      <p className="mb-4">
                        Create an ACME order to get your DNS challenge token.
                      </p>
                      <Button
                        onClick={handleCreateOrder}
                        disabled={createOrder.isPending}
                      >
                        {createOrder.isPending ? (
                          <>
                            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                            Creating...
                          </>
                        ) : (
                          <>
                            <Shield className="mr-2 h-4 w-4" />
                            Create Order
                          </>
                        )}
                      </Button>
                    </AlertDescription>
                  </Alert>
                )}

                {order && hasDnsValues && (
                  <>
                    {/* Error Alert - Show if any validation status is invalid or order has errors */}
                    {hasAnyError && (
                      <Alert variant="destructive">
                        <AlertTriangle className="h-4 w-4" />
                        <AlertTitle>Challenge Verification Failed</AlertTitle>
                        <AlertDescription>
                          <div className="space-y-3">
                            {/* Show validation errors for each record */}
                            {Array.from(recordValidations.entries()).map(
                              ([index, status]) =>
                                status?.error ? (
                                  <div key={index} className="space-y-2">
                                    <p className="text-sm font-semibold">
                                      {dnsTxtRecords.length > 1
                                        ? `Record ${index + 1} - `
                                        : ''}
                                      Let&apos;s Encrypt Validation Error:
                                    </p>
                                    <p className="text-sm">
                                      {status.error.detail}
                                    </p>
                                    {status.error.type && (
                                      <p className="text-xs font-mono bg-destructive/10 p-2 rounded">
                                        Error type: {status.error.type}
                                      </p>
                                    )}
                                    {status.validated && (
                                      <p className="text-xs text-muted-foreground">
                                        Validated at:{' '}
                                        {new Date(
                                          status.validated
                                        ).toLocaleString()}
                                      </p>
                                    )}
                                  </div>
                                ) : null
                            )}

                            {/* Fallback to order error if no validation error */}
                            {!hasAnyInvalidRecord && order.error && (
                              <>
                                <p className="text-sm">{order.error}</p>
                                {order.error_type && (
                                  <p className="text-xs font-mono bg-destructive/10 p-2 rounded">
                                    Error type: {order.error_type}
                                  </p>
                                )}
                              </>
                            )}

                            {/* Generic message if no specific error */}
                            {!hasAnyInvalidRecord && !order.error && (
                              <p className="text-sm">
                                The DNS challenge verification failed. The TXT
                                records may have incorrect values or the
                                challenge has expired.
                              </p>
                            )}

                            <div className="flex gap-2 mt-4">
                              <Button
                                variant="destructive"
                                size="sm"
                                onClick={handleCancelAndRecreate}
                                disabled={
                                  cancelOrder.isPending || createOrder.isPending
                                }
                              >
                                {cancelOrder.isPending ||
                                createOrder.isPending ? (
                                  <>
                                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                    Recreating Order...
                                  </>
                                ) : (
                                  <>
                                    <XCircle className="mr-2 h-4 w-4" />
                                    Restart Order
                                  </>
                                )}
                              </Button>
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => {
                                  // Refetch all validations
                                  dnsTxtRecords.forEach((record, index) => {
                                    const validationUrl =
                                      record.validation_url ||
                                      challengeData?.validation_url
                                    if (validationUrl) {
                                      fetchValidationStatus(
                                        validationUrl,
                                        index
                                      )
                                    }
                                  })
                                }}
                                disabled={fetchingRecords.size > 0}
                              >
                                {fetchingRecords.size > 0 ? (
                                  <>
                                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                    Checking...
                                  </>
                                ) : (
                                  <>
                                    <RefreshCw className="mr-2 h-4 w-4" />
                                    Check Status
                                  </>
                                )}
                              </Button>
                            </div>
                            <p className="text-xs text-muted-foreground">
                              Restarting will cancel the current order and
                              generate new DNS challenge tokens. Make sure to
                              update your DNS records with the new values.
                            </p>
                          </div>
                        </AlertDescription>
                      </Alert>
                    )}

                    {/* Step 1: Add DNS TXT Records */}
                    <Alert>
                      <Info className="h-4 w-4" />
                      <AlertTitle>Step 1: Add DNS TXT Record</AlertTitle>
                      <AlertDescription>
                        Add the following TXT record
                        {dnsTxtRecords.length > 1 ? 's' : ''} to your DNS
                        provider:
                      </AlertDescription>
                    </Alert>

                    <DnsTxtRecordsDisplay
                      records={dnsTxtRecords}
                      showPropagationLinks={true}
                    />

                    {/* Step 3: Verify & Complete */}
                    <Alert>
                      <CheckCircle className="h-4 w-4" />
                      <AlertTitle>Step 3: Verify & Complete</AlertTitle>
                      <AlertDescription>
                        <p className="mb-4">
                          Once the DNS record has propagated, click the button
                          below to verify and provision your SSL certificate.
                        </p>
                        <div className="flex gap-2">
                          <Button
                            onClick={handleCompleteDns}
                            disabled={finalizeOrder.isPending || hasAnyError}
                          >
                            {finalizeOrder.isPending ? (
                              <>
                                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                Verifying...
                              </>
                            ) : (
                              <>
                                <CheckCircle className="mr-2 h-4 w-4" />
                                Verify & Finalize
                              </>
                            )}
                          </Button>
                          <Button
                            variant="outline"
                            size="default"
                            onClick={handleCancelAndRecreate}
                            disabled={
                              cancelOrder.isPending || createOrder.isPending
                            }
                          >
                            {cancelOrder.isPending || createOrder.isPending ? (
                              <>
                                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                Restarting...
                              </>
                            ) : (
                              <>
                                <RefreshCw className="mr-2 h-4 w-4" />
                                Restart Order
                              </>
                            )}
                          </Button>
                          {showBackButton && onBack && (
                            <Button variant="outline" onClick={onBack}>
                              Back
                            </Button>
                          )}
                        </div>
                      </AlertDescription>
                    </Alert>
                  </>
                )}
              </div>
            )}

          {/* Active Certificate */}
          {domain.status === 'active' && (
            <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
              <CheckCircle2 className="h-4 w-4 text-green-600" />
              <AlertDescription>
                Your SSL certificate is active and your domain is secured with
                HTTPS.
              </AlertDescription>
            </Alert>
          )}
        </div>

        {/* Sidebar - ACME Order Information */}
        <div className="space-y-6">
          {order && (
            <AcmeOrderInfo
              order={order}
              dnsTxtRecords={dnsTxtRecords}
              onRefresh={() => refetchOrder()}
              showRefresh={true}
            />
          )}
        </div>
      </div>

      {/* Navigation buttons at bottom */}
      {domain.status === 'active' && showContinueButton && onSuccess && (
        <div className="flex justify-end">
          <Button onClick={() => onSuccess(domain)}>Continue</Button>
        </div>
      )}
    </div>
  )
}
