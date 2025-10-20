import { LoginForm } from '@/components/auth/login-form'
import { loginMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '@/contexts/AuthContext'

export const Login = () => {
  const [isLoading, setIsLoading] = useState(false)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { refetch } = useAuth()

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
      // Invalidate and refetch user data
      await queryClient.invalidateQueries({ queryKey: ['getCurrentUser'] })
      await refetch()
      // Navigate using React Router
      navigate('/dashboard')
    },
  })

  const handleSubmit = async (data: { email: string; password: string }) => {
    setIsLoading(true)
    try {
      await login.mutateAsync({
        body: data,
      })
    } catch (error) {
      // Error is handled in onError
    } finally {
      setIsLoading(false)
    }
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex flex-col items-center space-y-6">
          <div className="flex items-center gap-2">
            <div className="flex aspect-square size-12 items-center justify-center rounded-lg bg-primary">
              <img
                src="/favicon.png"
                alt="Temps logo"
                className="size-full rounded-lg"
              />
            </div>
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

        <LoginForm
          onSubmit={handleSubmit}
          isLoading={isLoading || login.isPending}
        />
      </div>
    </div>
  )
}
