import { LoginForm } from '@/components/auth/login-form'
import { loginMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '@/contexts/AuthContext'
import { Play } from 'lucide-react'

export const Login = () => {
  const [isLoading, setIsLoading] = useState(false)
  const [isDemoLoading, setIsDemoLoading] = useState(false)
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
    } finally {
      setIsLoading(false)
    }
  }

  const handleDemoLogin = async () => {
    setIsDemoLoading(true)
    try {
      const response = await fetch('/api/auth/demo', {
        method: 'POST',
        credentials: 'include',
      })

      if (!response.ok) {
        const error = await response.json()
        throw new Error(error.detail || 'Failed to start demo')
      }

      toast.success('Welcome to the demo!')
      // Invalidate and refetch user data
      await queryClient.invalidateQueries({ queryKey: ['getCurrentUser'] })
      await refetch()
      // Navigate to dashboard
      navigate('/dashboard')
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : 'Failed to start demo'
      )
    } finally {
      setIsDemoLoading(false)
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

        <div className="relative">
          <div className="absolute inset-0 flex items-center">
            <span className="w-full border-t" />
          </div>
          <div className="relative flex justify-center text-xs uppercase">
            <span className="bg-background px-2 text-muted-foreground">
              Or continue with
            </span>
          </div>
        </div>

        <Button
          variant="outline"
          onClick={handleDemoLogin}
          disabled={isDemoLoading}
          className="w-full"
        >
          <Play className="mr-2 h-4 w-4" />
          {isDemoLoading ? 'Starting demo...' : 'Try Demo'}
        </Button>
        <p className="text-center text-xs text-muted-foreground">
          Explore analytics and monitoring with sample data
        </p>
      </div>
    </div>
  )
}
