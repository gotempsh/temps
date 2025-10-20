'use client'

import { verifyMfaChallengeMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { useAuth } from '@/contexts/AuthContext'
import * as z from 'zod'

const formSchema = z.object({
  code: z
    .string()
    .min(6, 'Code must be 6 digits')
    .max(6, 'Code must be 6 digits'),
})

type FormValues = z.infer<typeof formSchema>

export const MfaVerify = () => {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { refetch } = useAuth()

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      code: '',
    },
  })

  const verifyMfaChallenge = useMutation({
    ...verifyMfaChallengeMutation(),
    meta: {
      errorTitle: 'MFA verification failed',
    },
    onSuccess: async () => {
      toast.success('MFA verified successfully')
      // Invalidate and refetch user data
      await queryClient.invalidateQueries({ queryKey: ['getCurrentUser'] })
      await refetch()
      // Navigate using React Router
      navigate('/dashboard')
    },
  })
  const onSubmit = async (data: FormValues) => {
    verifyMfaChallenge.mutate({
      body: {
        code: data.code,
      },
    })
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background">
      <div className="mx-auto flex w-full flex-col justify-center space-y-6 sm:w-[350px]">
        <div className="flex flex-col space-y-2 text-center">
          <h1 className="text-2xl font-semibold tracking-tight">
            Two-factor authentication
          </h1>
          <p className="text-sm text-muted-foreground">
            Enter the 6-digit code from your authenticator app
          </p>
        </div>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            <FormField
              control={form.control}
              name="code"
              render={({ field }) => (
                <FormItem>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="000000"
                      type="text"
                      maxLength={6}
                      className="text-center text-lg tracking-widest"
                      autoComplete="one-time-code"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button
              type="submit"
              className="w-full"
              disabled={verifyMfaChallenge.isPending}
            >
              {verifyMfaChallenge.isPending ? 'Verifying...' : 'Verify'}
            </Button>
          </form>
        </Form>

        <p className="px-8 text-center text-sm text-muted-foreground">
          Didn&apos;t receive a code?{' '}
          <button
            onClick={() => {
              navigate('/')
            }}
            className="underline underline-offset-4 hover:text-primary"
          >
            Go back to login
          </button>
        </p>
      </div>
    </div>
  )
}
