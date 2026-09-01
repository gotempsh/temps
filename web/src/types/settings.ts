// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface SecuritySetting {
  id: string
  title: string
  description: string
  enabled: boolean
  lastUpdated?: Date
  updatedBy?: string
}

export interface SecuritySettingsResponse {
  settings: SecuritySetting[]
}

export interface UpdateSecuritySettingRequest {
  settingId: string
  enabled: boolean
}
