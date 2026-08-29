// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CreatableServiceTypeRoute } from '@/api/client/types.gen'
import { ADD_SERVICE_TYPES } from '@/lib/addServiceTypes'

export interface TemplateServiceReference {
  id: number
  name: string
  service_type: string
}

export interface TemplateServiceRequirement {
  key: string
  label: string
  serviceType?: CreatableServiceTypeRoute
  availableServices: TemplateServiceReference[]
  selectedServices: TemplateServiceReference[]
  isSatisfied: boolean
}

const SERVICE_TYPE_ALIASES: Record<string, string> = {
  postgresql: 'postgres',
  mysql: 'mariadb',
  object_storage: 's3',
  'object-storage': 's3',
}

export function normalizeTemplateServiceType(serviceType: string): string {
  const normalized = serviceType.trim().toLowerCase()
  return SERVICE_TYPE_ALIASES[normalized] ?? normalized
}

export function getTemplateServiceRequirements(
  requiredServiceTypes: string[],
  availableServices: TemplateServiceReference[],
  selectedServiceIds: number[]
): TemplateServiceRequirement[] {
  const selectedIds = new Set(selectedServiceIds)
  const uniqueRequirements = Array.from(
    new Set(requiredServiceTypes.map(normalizeTemplateServiceType))
  ).filter(Boolean)

  return uniqueRequirements.map((key) => {
    const serviceOption = ADD_SERVICE_TYPES.find((option) => option.id === key)
    const matchingServices = availableServices.filter(
      (service) => normalizeTemplateServiceType(service.service_type) === key
    )
    const selectedServices = matchingServices.filter((service) =>
      selectedIds.has(service.id)
    )

    return {
      key,
      label: serviceOption?.name ?? key,
      serviceType: serviceOption?.id,
      availableServices: matchingServices,
      selectedServices,
      isSatisfied: selectedServices.length > 0,
    }
  })
}
