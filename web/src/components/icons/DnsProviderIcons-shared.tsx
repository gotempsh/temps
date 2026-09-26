// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Globe } from 'lucide-react'
import {
  CloudflareIcon,
  AwsRoute53Icon,
  GoogleCloudIcon,
  AzureIcon,
  DigitalOceanIcon,
  NamecheapIcon,
} from './DnsProviderIcons-components'

export type IconProps = { className?: string }

export function getDnsProviderIcon(
  providerType: string,
  className = 'h-4 w-4'
) {
  switch (providerType.toLowerCase()) {
    case 'cloudflare':
      return <CloudflareIcon className={className} />
    case 'route53':
      return <AwsRoute53Icon className={className} />
    case 'gcp':
      return <GoogleCloudIcon className={className} />
    case 'azure':
      return <AzureIcon className={className} />
    case 'digitalocean':
      return <DigitalOceanIcon className={className} />
    case 'namecheap':
      return <NamecheapIcon className={className} />
    default:
      return <Globe className={className} />
  }
}
