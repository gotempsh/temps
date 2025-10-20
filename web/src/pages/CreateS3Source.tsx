'use client'

import { createS3SourceMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useMutation } from '@tanstack/react-query'
import { ArrowLeft, Plus } from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

interface NewS3Source {
  name: string
  bucket_name: string
  region: string
  access_key_id: string
  secret_key: string
  endpoint?: string
  force_path_style?: boolean
}

export function CreateS3Source() {
  const navigate = useNavigate()
  const [formData, setFormData] = useState<Partial<NewS3Source>>({
    force_path_style: false,
  })

  const createMutation = useMutation({
    ...createS3SourceMutation(),
    meta: {
      errorTitle: 'Failed to create S3 source',
    },
    onSuccess: () => {
      toast.success('S3 source created successfully')
      navigate('/backups')
    },
  })

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    if (
      !formData.name ||
      !formData.bucket_name ||
      !formData.region ||
      !formData.access_key_id ||
      !formData.secret_key
    ) {
      toast.error('Please fill in all required fields')
      return
    }

    createMutation.mutate({
      body: {
        ...(formData as NewS3Source),
        bucket_path: '/',
      },
    })
  }

  const isFormValid =
    formData.name &&
    formData.bucket_name &&
    formData.region &&
    formData.access_key_id &&
    formData.secret_key

  return (
    <div className="container mx-auto max-w-2xl py-6">
      <div className="mb-6">
        <div className="flex items-center gap-2 mb-2">
          <Link
            to="/backups"
            className="flex items-center text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="h-4 w-4" />
          </Link>
          <h1 className="text-2xl font-semibold">Add S3 Source</h1>
        </div>
        <p className="text-muted-foreground">
          Configure a new S3 storage source for your backups
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Plus className="h-5 w-5" />
            S3 Configuration
          </CardTitle>
          <CardDescription>
            Enter your S3 credentials and configuration. All fields marked with
            * are required.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-6">
            <div className="grid gap-4">
              <div className="grid gap-2">
                <Label htmlFor="name">Source Name *</Label>
                <Input
                  id="name"
                  placeholder="Backup Storage"
                  value={formData.name || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, name: e.target.value })
                  }
                  required
                />
              </div>

              <div className="grid gap-2">
                <Label htmlFor="bucket">Bucket Name *</Label>
                <Input
                  id="bucket"
                  placeholder="my-backups"
                  value={formData.bucket_name || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, bucket_name: e.target.value })
                  }
                  required
                />
              </div>

              <div className="grid gap-2">
                <Label htmlFor="region">Region *</Label>
                <Input
                  id="region"
                  placeholder="us-east-1"
                  value={formData.region || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, region: e.target.value })
                  }
                  required
                />
              </div>

              <div className="grid gap-2">
                <Label
                  htmlFor="endpoint"
                  className="flex items-baseline justify-between"
                >
                  <span>Endpoint URL</span>
                  <span className="text-xs text-muted-foreground">
                    (Optional, for MinIO)
                  </span>
                </Label>
                <Input
                  id="endpoint"
                  placeholder="http://minio.example.com:9000"
                  value={formData.endpoint || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, endpoint: e.target.value })
                  }
                />
              </div>

              <div className="grid gap-2">
                <Label
                  htmlFor="forcePathStyle"
                  className="flex items-center space-x-2"
                >
                  <Input
                    id="forcePathStyle"
                    type="checkbox"
                    className="h-4 w-4"
                    checked={formData.force_path_style || false}
                    onChange={(e) =>
                      setFormData({
                        ...formData,
                        force_path_style: e.target.checked,
                      })
                    }
                  />
                  <div>
                    <span>Force Path Style</span>
                    <p className="text-xs text-muted-foreground">
                      Enable for MinIO compatibility
                    </p>
                  </div>
                </Label>
              </div>

              <div className="grid gap-2">
                <Label htmlFor="accessKeyId">Access Key ID *</Label>
                <Input
                  id="accessKeyId"
                  type="password"
                  placeholder="AKIAXXXXXXXXXXXXXXXX"
                  value={formData.access_key_id || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, access_key_id: e.target.value })
                  }
                  required
                />
              </div>

              <div className="grid gap-2">
                <Label htmlFor="secretKey">Secret Key *</Label>
                <Input
                  id="secretKey"
                  type="password"
                  placeholder="Enter your AWS secret key"
                  value={formData.secret_key || ''}
                  onChange={(e) =>
                    setFormData({ ...formData, secret_key: e.target.value })
                  }
                  required
                />
              </div>
            </div>

            <div className="flex items-center justify-between pt-4">
              <Button
                type="button"
                variant="outline"
                onClick={() => navigate('/backups')}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={createMutation.isPending || !isFormValid}
              >
                {createMutation.isPending ? 'Creating...' : 'Create S3 Source'}
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
