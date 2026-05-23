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
    return searchParams.get('reason') ?? 'SSO sign-in failed'
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
