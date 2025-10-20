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
