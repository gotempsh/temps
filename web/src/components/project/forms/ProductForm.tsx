import { Button } from '@/components/ui/button'
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
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import * as z from 'zod'
import { cn } from '@/lib/utils'

export const productFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  type: z.enum(['subscription', 'one-time'], {
    required_error: 'You need to select a product type',
  }),
  price: z.string().regex(/^\$?\d+(\.\d{2})?$/, 'Invalid price format'),
  billingPeriod: z.enum(['monthly', 'yearly']).optional(),
})

export type ProductFormValues = z.infer<typeof productFormSchema>

interface ProductFormProps {
  initialData?: Partial<ProductFormValues>
  onSubmit: (data: ProductFormValues) => void
  onCancel: () => void
  disabled?: boolean
}

export function ProductForm({
  initialData,
  onSubmit,
  onCancel,
  disabled,
}: ProductFormProps) {
  const form = useForm<ProductFormValues>({
    resolver: zodResolver(productFormSchema),
    defaultValues: initialData || {
      name: '',
      type: 'subscription',
      price: '',
      billingPeriod: 'monthly',
    },
  })

  const productType = form.watch('type')

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Product Name</FormLabel>
              <FormControl>
                <Input placeholder="Enter product name" {...field} />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="type"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Product Type</FormLabel>
              <FormControl>
                <RadioGroup
                  onValueChange={field.onChange}
                  defaultValue={field.value}
                  className="flex flex-col space-y-1"
                  disabled={!!initialData}
                >
                  <FormItem className="flex items-center space-x-3 space-y-0">
                    <FormControl>
                      <RadioGroupItem
                        value="subscription"
                        disabled={!!initialData}
                      />
                    </FormControl>
                    <FormLabel
                      className={cn(
                        'font-normal',
                        initialData && 'text-muted-foreground'
                      )}
                    >
                      Subscription
                    </FormLabel>
                  </FormItem>
                  <FormItem className="flex items-center space-x-3 space-y-0">
                    <FormControl>
                      <RadioGroupItem
                        value="one-time"
                        disabled={!!initialData}
                      />
                    </FormControl>
                    <FormLabel
                      className={cn(
                        'font-normal',
                        initialData && 'text-muted-foreground'
                      )}
                    >
                      One-time payment
                    </FormLabel>
                  </FormItem>
                </RadioGroup>
              </FormControl>
              {initialData && (
                <p className="text-sm text-muted-foreground mt-1">
                  Product type cannot be changed after creation
                </p>
              )}
              <FormMessage />
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="price"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Price</FormLabel>
              <FormControl>
                <Input placeholder="$0.00" {...field} />
              </FormControl>
              <FormDescription>
                Enter the price in USD (e.g. $99.99)
              </FormDescription>
              <FormMessage />
            </FormItem>
          )}
        />

        {productType === 'subscription' && (
          <FormField
            control={form.control}
            name="billingPeriod"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Billing Period</FormLabel>
                <FormControl>
                  <RadioGroup
                    onValueChange={field.onChange}
                    defaultValue={field.value}
                    className="flex flex-col space-y-1"
                  >
                    <FormItem className="flex items-center space-x-3 space-y-0">
                      <FormControl>
                        <RadioGroupItem value="monthly" />
                      </FormControl>
                      <FormLabel className="font-normal">Monthly</FormLabel>
                    </FormItem>
                    <FormItem className="flex items-center space-x-3 space-y-0">
                      <FormControl>
                        <RadioGroupItem value="yearly" />
                      </FormControl>
                      <FormLabel className="font-normal">Yearly</FormLabel>
                    </FormItem>
                  </RadioGroup>
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
        )}

        <div className="flex justify-end space-x-4">
          <Button variant="outline" type="button" onClick={onCancel}>
            Cancel
          </Button>
          <Button type="submit" disabled={disabled}>
            {initialData ? 'Update Product' : 'Create Product'}
          </Button>
        </div>
      </form>
    </Form>
  )
}
