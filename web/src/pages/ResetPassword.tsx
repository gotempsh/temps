import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { resetPasswordMutation } from '@/api/client/@tanstack/react-query.gen'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Loader2 } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'
import { usePageTitle } from '@/hooks/usePageTitle'

// Mirrors `validate_password_complexity` in
// temps-auth/src/auth_service.rs. Kept in sync so the user gets inline
// feedback instead of a round-trip 400. The server remains the source
// of truth and re-validates.
const resetPasswordSchema = z
  .object({
    newPassword: z
      .string()
      .min(8, 'Password must be at least 8 characters long')
      .max(128, 'Password must not exceed 128 characters')
      .regex(/[A-Z]/, 'Password must contain at least one uppercase letter')
      .regex(/[a-z]/, 'Password must contain at least one lowercase letter')
      .regex(/[0-9]/, 'Password must contain at least one digit')
      .regex(
        /[^a-zA-Z0-9]/,
        'Password must contain at least one special character',
      ),
    confirmPassword: z.string(),
  })
  .refine((data) => data.newPassword === data.confirmPassword, {
    message: 'Passwords do not match',
    path: ['confirmPassword'],
  })

type ResetPasswordFormData = z.infer<typeof resetPasswordSchema>

export const ResetPassword = () => {
  usePageTitle('Reset password')
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  const form = useForm<ResetPasswordFormData>({
    resolver: zodResolver(resetPasswordSchema),
    defaultValues: { newPassword: '', confirmPassword: '' },
  })

  const resetPassword = useMutation({
    ...resetPasswordMutation(),
    meta: { errorTitle: 'Password reset failed' },
    onSuccess: () => {
      toast.success('Password reset. You can now log in.')
      // Root renders the login screen when logged out (there is no
      // dedicated /login route — ProtectedLayout shows <Login /> on the
      // unauthenticated root).
      navigate('/', { replace: true })
    },
  })

  const handleSubmit = async (data: ResetPasswordFormData) => {
    await resetPassword.mutateAsync({
      body: { token, new_password: data.newPassword },
    })
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
        </div>

        {token ? (
          <Card className="w-full max-w-sm">
            <CardHeader>
              <CardTitle className="text-2xl">Set a new password</CardTitle>
              <CardDescription>
                Choose a strong password you haven't used before.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Form {...form}>
                <form
                  onSubmit={form.handleSubmit(handleSubmit)}
                  className="space-y-4"
                >
                  <FormField
                    control={form.control}
                    name="newPassword"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>New password</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="Enter a new password"
                            autoComplete="new-password"
                            disabled={resetPassword.isPending}
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="confirmPassword"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Confirm password</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="Re-enter your new password"
                            autoComplete="new-password"
                            disabled={resetPassword.isPending}
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <Button
                    type="submit"
                    className="w-full"
                    disabled={resetPassword.isPending}
                  >
                    {resetPassword.isPending ? (
                      <>
                        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                        Resetting...
                      </>
                    ) : (
                      'Reset password'
                    )}
                  </Button>
                </form>
              </Form>
            </CardContent>
          </Card>
        ) : (
          <Card className="w-full max-w-sm">
            <CardHeader>
              <CardTitle className="text-2xl">Invalid reset link</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertTitle>Missing token</AlertTitle>
                <AlertDescription>
                  This reset link is missing its token. Request a new link and
                  try again.
                </AlertDescription>
              </Alert>
              <Button asChild variant="outline" className="w-full">
                <Link to="/forgot-password">Request a new link</Link>
              </Button>
            </CardContent>
          </Card>
        )}

        <Button asChild variant="ghost" className="w-full">
          <Link to="/">
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to login
          </Link>
        </Button>
      </div>
    </div>
  )
}
