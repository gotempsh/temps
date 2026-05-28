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
import {
  emailStatusOptions,
  requestPasswordResetMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Loader2, MailCheck } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router-dom'
import { z } from 'zod'
import { usePageTitle } from '@/hooks/usePageTitle'

const forgotPasswordSchema = z.object({
  email: z.email('Please enter a valid email address'),
})

type ForgotPasswordFormData = z.infer<typeof forgotPasswordSchema>

export const ForgotPassword = () => {
  usePageTitle('Forgot password')

  // The server only offers password reset when an email provider is
  // configured (request_password_reset returns 503 otherwise). Surface
  // that up front so the user isn't told "check your inbox" for an email
  // that will never arrive.
  const { data: emailStatus } = useQuery(emailStatusOptions())
  const resetAvailable = emailStatus?.password_reset_available ?? true

  const form = useForm<ForgotPasswordFormData>({
    resolver: zodResolver(forgotPasswordSchema),
    defaultValues: { email: '' },
  })

  // The request endpoint is enumeration-safe: it returns 200 whether or
  // not the account exists. The only error path is 503 (email provider
  // not configured), which the global mutation handler toasts — and we
  // also pre-gate the form on `resetAvailable` so it rarely fires.
  const requestReset = useMutation({
    ...requestPasswordResetMutation(),
    meta: { errorTitle: 'Could not send reset link' },
  })

  const handleSubmit = async (data: ForgotPasswordFormData) => {
    await requestReset.mutateAsync({ body: { email: data.email } })
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

        {requestReset.isSuccess ? (
          <Card className="w-full max-w-sm">
            <CardHeader>
              <div className="mb-2 flex size-10 items-center justify-center rounded-full bg-primary/10">
                <MailCheck className="size-5 text-primary" />
              </div>
              <CardTitle className="text-2xl">Check your email</CardTitle>
              <CardDescription>
                If an account exists for{' '}
                <span className="font-medium text-foreground">
                  {form.getValues('email')}
                </span>
                , we've sent a link to reset your password. The link expires in
                1 hour.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Button asChild variant="outline" className="w-full">
                <Link to="/">
                  <ArrowLeft className="mr-2 h-4 w-4" />
                  Back to login
                </Link>
              </Button>
            </CardContent>
          </Card>
        ) : (
          <Card className="w-full max-w-sm">
            <CardHeader>
              <CardTitle className="text-2xl">Forgot password?</CardTitle>
              <CardDescription>
                Enter your email and we'll send you a link to reset your
                password.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {!resetAvailable && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertTitle>Password reset unavailable</AlertTitle>
                  <AlertDescription>
                    No email provider is configured on this server, so reset
                    links can't be sent. Contact your administrator.
                  </AlertDescription>
                </Alert>
              )}

              <Form {...form}>
                <form
                  onSubmit={form.handleSubmit(handleSubmit)}
                  className="space-y-4"
                >
                  <FormField
                    control={form.control}
                    name="email"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Email</FormLabel>
                        <FormControl>
                          <Input
                            type="email"
                            placeholder="you@example.com"
                            autoComplete="email"
                            disabled={requestReset.isPending || !resetAvailable}
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
                    disabled={requestReset.isPending || !resetAvailable}
                  >
                    {requestReset.isPending ? (
                      <>
                        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                        Sending...
                      </>
                    ) : (
                      'Send reset link'
                    )}
                  </Button>
                </form>
              </Form>

              <Button asChild variant="ghost" className="w-full">
                <Link to="/">
                  <ArrowLeft className="mr-2 h-4 w-4" />
                  Back to login
                </Link>
              </Button>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  )
}
