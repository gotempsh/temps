import {
  disableMfaMutation,
  getCurrentUserOptions,
  setupMfaMutation,
  updateSelfMutation,
  verifyAndEnableMfaMutation,
} from '@/api/client/@tanstack/react-query.gen'
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
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { MfaSetupResponse } from '@/api/client'
import { useAuth } from '@/contexts/AuthContext'

const formSchema = z.object({
  name: z.string().min(2, 'Name must be at least 2 characters'),
  email: z.string().email('Invalid email address'),
})

type FormValues = z.infer<typeof formSchema>

const mfaVerifySchema = z.object({
  code: z.string().length(6, 'Code must be 6 digits'),
})

type MfaVerifyValues = z.infer<typeof mfaVerifySchema>

const mfaDisableSchema = z.object({
  code: z.string().length(6, 'Code must be 6 digits'),
})

type MfaDisableValues = z.infer<typeof mfaDisableSchema>

export function Account() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()

  const { data: user, isLoading } = useQuery({
    ...getCurrentUserOptions(),
  })
  const { refetch } = useAuth()
  const [showMfaDialog, setShowMfaDialog] = useState(false)
  const [mfaSetupData, setMfaSetupData] = useState<MfaSetupResponse | null>(
    null
  )
  const [showDisableMfaDialog, setShowDisableMfaDialog] = useState(false)

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: user
      ? {
          name: user.name,
          email: user.email ?? '',
        }
      : {
          name: '',
          email: '',
        },
  })

  const { mutate: updateUser, isPending } = useMutation({
    ...updateSelfMutation(),
    meta: {
      errorTitle: 'Failed to update account',
    },
    onSuccess: () => {
      toast.success('Account updated successfully')
      refetch()
    },
  })

  const mfaForm = useForm<MfaVerifyValues>({
    resolver: zodResolver(mfaVerifySchema),
    defaultValues: {
      code: '',
    },
  })

  const { mutate: setupMfa, isPending: isSettingUpMfa } = useMutation({
    ...setupMfaMutation(),
    meta: {
      errorTitle: 'Failed to setup MFA',
    },
    onSuccess: (data) => {
      setMfaSetupData(data)
      setShowMfaDialog(true)
    },
  })

  const { mutate: verifyMfa, isPending: isVerifyingMfa } = useMutation({
    ...verifyAndEnableMfaMutation(),
    meta: {
      errorTitle: 'Failed to enable MFA',
    },
    onSuccess: () => {
      toast.success('MFA enabled successfully')
      setShowMfaDialog(false)
      refetch()
    },
  })

  const mfaDisableForm = useForm<MfaDisableValues>({
    resolver: zodResolver(mfaDisableSchema),
    defaultValues: {
      code: '',
    },
  })

  const { mutate: disableMfa, isPending: isDisablingMfa } = useMutation({
    ...disableMfaMutation(),
    meta: {
      errorTitle: 'Failed to disable MFA',
    },
    onSuccess: () => {
      toast.success('MFA disabled successfully')
      setShowDisableMfaDialog(false)
      refetch()
      queryClient.invalidateQueries({
        queryKey: getCurrentUserOptions().queryKey,
      })
      mfaDisableForm.reset()
    },
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Account' }])
  }, [setBreadcrumbs])

  usePageTitle('Account')

  function onSubmit(data: FormValues) {
    updateUser({
      body: data,
    })
  }

  const onVerifyMfa = (data: MfaVerifyValues) => {
    verifyMfa({
      body: { code: data.code },
    })
  }

  const onDisableMfa = (data: MfaDisableValues) => {
    disableMfa({
      body: { code: data.code },
    })
  }

  if (isLoading) {
    return <AccountSkeleton />
  }

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Account Settings</CardTitle>
          <CardDescription>Manage your account information</CardDescription>
        </CardHeader>
        <CardContent>
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="email"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Email</FormLabel>
                    <FormControl>
                      <Input {...field} type="email" />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <div className="flex justify-end">
                <Button type="submit" disabled={isPending}>
                  {isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Save Changes
                </Button>
              </div>
            </form>
          </Form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Two-Factor Authentication</CardTitle>
          <CardDescription>
            Add an extra layer of security to your account by enabling
            two-factor authentication
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {user?.mfa_enabled ? (
            <div className="space-y-4">
              <Alert>
                <AlertDescription>
                  Two-factor authentication is currently enabled for your
                  account.
                </AlertDescription>
              </Alert>
              <Button
                variant="destructive"
                onClick={() => setShowDisableMfaDialog(true)}
                disabled={isDisablingMfa}
              >
                {isDisablingMfa && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Disable 2FA
              </Button>
            </div>
          ) : (
            <Button onClick={() => setupMfa({})} disabled={isSettingUpMfa}>
              {isSettingUpMfa && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Setup 2FA
            </Button>
          )}
        </CardContent>
      </Card>

      <Dialog open={showMfaDialog} onOpenChange={setShowMfaDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Setup Two-Factor Authentication</DialogTitle>
            <DialogDescription>
              Scan the QR code with your authenticator app and enter the
              verification code below.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            {mfaSetupData?.qr_code && (
              <div className="flex justify-center">
                <img
                  src={mfaSetupData.qr_code}
                  alt="QR Code for 2FA"
                  className="w-48 h-48"
                />
              </div>
            )}
            <div className="text-sm text-muted-foreground text-center">
              If you can&apos;t scan the QR code, enter this code manually:
              <br />
              <code className="font-mono bg-muted px-2 py-1 rounded">
                {mfaSetupData?.secret_key}
              </code>
            </div>
            <Form {...mfaForm}>
              <form
                onSubmit={mfaForm.handleSubmit(onVerifyMfa)}
                className="space-y-4"
              >
                <FormField
                  control={mfaForm.control}
                  name="code"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Verification Code</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="Enter 6-digit code" />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <div className="flex justify-end">
                  <Button type="submit" disabled={isVerifyingMfa}>
                    {isVerifyingMfa && (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    )}
                    Verify and Enable
                  </Button>
                </div>
              </form>
            </Form>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog
        open={showDisableMfaDialog}
        onOpenChange={setShowDisableMfaDialog}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Disable Two-Factor Authentication</DialogTitle>
            <DialogDescription>
              Please enter your 2FA code to confirm disabling two-factor
              authentication. This will make your account less secure.
            </DialogDescription>
          </DialogHeader>
          <Form {...mfaDisableForm}>
            <form
              onSubmit={mfaDisableForm.handleSubmit(onDisableMfa)}
              className="space-y-4"
            >
              <FormField
                control={mfaDisableForm.control}
                name="code"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Verification Code</FormLabel>
                    <FormControl>
                      <Input {...field} placeholder="Enter 6-digit code" />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <div className="flex justify-end gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    setShowDisableMfaDialog(false)
                    mfaDisableForm.reset()
                  }}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  variant="destructive"
                  disabled={isDisablingMfa}
                >
                  {isDisablingMfa && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Disable 2FA
                </Button>
              </div>
            </form>
          </Form>
        </DialogContent>
      </Dialog>
    </div>
  )
}

function AccountSkeleton() {
  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <Card>
        <CardHeader>
          <Skeleton className="h-8 w-[200px]" />
          <Skeleton className="h-4 w-[300px]" />
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-2">
            <Skeleton className="h-4 w-[100px]" />
            <Skeleton className="h-10 w-full" />
          </div>
          <div className="space-y-2">
            <Skeleton className="h-4 w-[100px]" />
            <Skeleton className="h-10 w-full" />
          </div>
          <div className="flex justify-end">
            <Skeleton className="h-10 w-[120px]" />
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
