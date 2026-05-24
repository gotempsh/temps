import { LoginForm } from '@/components/auth/login-form'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  emailStatusOptions,
  loginMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { consumeReturnTo } from '@/lib/return-to'

/**
 * Maps opaque SSO error codes (from `login_error_code_for` in
 * `oidc_handler.rs`) to user-facing messages. Server returns codes
 * instead of raw IdP text so we don't leak IdP error descriptions
 * into the browser URL / history / Referer. Unknown codes fall
 * through to a generic message.
 */
const OIDC_ERROR_MESSAGES: Record<string, string> = {
  idp_error: 'Your identity provider rejected the login. Check that your account is allowed.',
  idp_unreachable: "We couldn't reach your identity provider. Try again in a moment.",
  idp_rejected_code: 'Your identity provider rejected the authorization code. Try signing in again.',
  state_invalid: 'This SSO link is invalid or has already been used. Start sign-in again.',
  state_expired: 'This SSO link expired. Start sign-in again.',
  id_token_invalid: 'Your identity provider returned an invalid token. Contact your administrator.',
  callback_invalid: 'The SSO callback was malformed. Start sign-in again.',
  email_missing: 'Your identity provider did not return an email address. Grant the "email" scope and try again.',
  email_not_verified: 'Your identity provider has not confirmed your email. Verify it at the IdP, then try again.',
  user_not_provisioned: 'No Temps account exists for this email. Ask an administrator to create one.',
  provider_disabled: 'This SSO provider is currently disabled.',
  provider_not_found: 'The SSO provider configuration was not found.',
  no_provider_configured: 'No SSO provider is configured on this Temps instance.',
  issuer_invalid: 'The SSO provider URL is invalid.',
  return_to_invalid: 'Invalid post-login redirect target.',
  role_invalid: 'The role assigned by the SSO provider is invalid.',
  role_mapping_not_found: 'No matching SSO role mapping.',
  provider_conflict: 'SSO provider configuration conflict.',
  internal_error: 'An internal error occurred while processing the SSO callback.',
}

function oidcErrorMessage(reason: string | null): string {
  if (!reason) return 'SSO sign-in failed.'
  return OIDC_ERROR_MESSAGES[reason] ?? 'SSO sign-in failed.'
}

export const Login = () => {
  usePageTitle('Login')
  const [isLoading, setIsLoading] = useState(false)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { refetch } = useAuth()
  const [searchParams] = useSearchParams()

  const { data: emailStatus } = useQuery(emailStatusOptions())

  const oidcError = useMemo(() => {
    if (searchParams.get('error') !== 'oidc_failed') {
      return null
    }
    // The server returns short opaque codes via `?reason=` (see
    // `login_error_code_for` in `oidc_handler.rs`) so we don't leak
    // raw IdP error text into the browser address bar / history /
    // Referer. Translate each known code into a user-facing message
    // here; unknown values fall through to a generic string.
    const reason = searchParams.get('reason')
    return oidcErrorMessage(reason)
  }, [searchParams])

  const login = useMutation({
    ...loginMutation(),
    meta: {
      errorTitle: 'Login failed',
    },
    onSuccess: async (data) => {
      if (data.mfa_required) {
        toast.success('Please complete MFA verification')
        navigate('/mfa-verify')
        return
      }

      toast.success('Logged in successfully')
      await queryClient.invalidateQueries({ queryKey: ['getCurrentUser'] })
      await refetch()
      navigate(consumeReturnTo('/dashboard'), { replace: true })
    },
  })

  const handleSubmit = async (data: { email: string; password: string }) => {
    setIsLoading(true)
    try {
      await login.mutateAsync({
        body: data,
      })
    } finally {
      setIsLoading(false)
    }
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex flex-col items-center space-y-6">
          <div className="flex items-center gap-3">
            <img
              src="/svg/temps-icon.svg"
              alt="Temps logo"
              className="size-12"
            />
            <span className="text-2xl font-bold">Temps</span>
          </div>
          <div className="flex flex-col space-y-2 text-center">
            <h1 className="text-2xl font-semibold tracking-tight">
              Welcome back
            </h1>
            <p className="text-sm text-muted-foreground">
              Sign in to your account to continue
            </p>
          </div>
        </div>

        {oidcError && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>SSO sign-in failed</AlertTitle>
            <AlertDescription>{oidcError}</AlertDescription>
          </Alert>
        )}

        <LoginForm
          onSubmit={handleSubmit}
          isLoading={isLoading || login.isPending}
          oidcProviders={emailStatus?.oidc_providers ?? []}
        />
      </div>
    </div>
  )
}
