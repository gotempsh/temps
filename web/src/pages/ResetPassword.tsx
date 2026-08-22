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
  FormDescription,
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
import { AlertCircle, ArrowLeft, Check, Loader2, X } from 'lucide-react'
import { useRef } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import {
  PASSWORD_REQUIREMENTS,
  passwordRequirementResults,
  passwordSchema,
} from '@/lib/password-policy'

const resetPasswordSchema = z
  .object({
    newPassword: passwordSchema,
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
  const isSubmittingRef = useRef(false)

  const form = useForm<ResetPasswordFormData>({
    resolver: zodResolver(resetPasswordSchema),
    defaultValues: { newPassword: '', confirmPassword: '' },
  })
  const newPassword = form.watch('newPassword')
  const requirementResults = passwordRequirementResults(newPassword)

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
    if (isSubmittingRef.current || resetPassword.isPending) return
    isSubmittingRef.current = true
    try {
      await resetPassword.mutateAsync({
        body: { token, new_password: data.newPassword },
      })
    } finally {
      isSubmittingRef.current = false
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
                            disabled={
                              resetPassword.isPending ||
                              form.formState.isSubmitting
                            }
                            {...field}
                          />
                        </FormControl>
                        <FormDescription className="sr-only">
                          Password must meet all complexity requirements listed
                          below.
                        </FormDescription>
                        <FormMessage />
                        <ul
                          role="list"
                          aria-live="polite"
                          aria-label="Password requirements"
                          className="grid gap-1.5 pt-2 sm:grid-cols-2"
                        >
                          {PASSWORD_REQUIREMENTS.map((requirement, index) => {
                            const met =
                              requirementResults[index]?.met ?? false
                            return (
                              <li
                                key={requirement.id}
                                className={cn(
                                  'flex items-center gap-1.5 text-xs transition-colors',
                                  met
                                    ? 'text-emerald-600 dark:text-emerald-400 font-medium'
                                    : 'text-rose-500 font-medium'
                                )}
                              >
                                {met ? (
                                  <Check
                                    aria-hidden="true"
                                    className="h-3.5 w-3.5 shrink-0 stroke-[2.5]"
                                  />
                                ) : (
                                  <X
                                    aria-hidden="true"
                                    className="h-3.5 w-3.5 shrink-0 stroke-[2.5]"
                                  />
                                )}
                                <span>{requirement.label}</span>
                                <span className="sr-only">
                                  {met
                                    ? '(Requirement met)'
                                    : '(Requirement not met)'}
                                </span>
                              </li>
                            )
                          })}
                        </ul>
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
                            disabled={
                              resetPassword.isPending ||
                              form.formState.isSubmitting
                            }
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
                    disabled={
                      resetPassword.isPending || form.formState.isSubmitting
                    }
                  >
                    {resetPassword.isPending ||
                    form.formState.isSubmitting ? (
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
