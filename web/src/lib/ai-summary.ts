export function shouldRequestApiTrafficSummary({
  requested,
  dataReady,
  consentEnabled,
}: {
  requested: boolean
  dataReady: boolean
  consentEnabled: boolean
}): boolean {
  return requested && dataReady && consentEnabled
}
