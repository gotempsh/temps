import React, { useState } from 'react'
import { Key, Globe } from 'lucide-react'

export type GitHubIntegrationMethod = 'pat' | 'app'

interface GitHubIntegrationMethodProps {
  domain: string
  onMethodSelect: (method: GitHubIntegrationMethod) => void
  onBack?: () => void
}

export const GitHubIntegrationMethod: React.FC<
  GitHubIntegrationMethodProps
> = ({ domain, onMethodSelect, onBack }) => {
  const [selectedMethod, setSelectedMethod] =
    useState<GitHubIntegrationMethod | null>(null)

  const handleMethodClick = (method: GitHubIntegrationMethod) => {
    setSelectedMethod(method)
    onMethodSelect(method)
  }

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <div className="text-center">
        <h2 className="text-2xl font-bold mb-2">Choose Integration Method</h2>
        <p className="text-muted-foreground">
          Select how you want to connect to {domain}
        </p>
      </div>

      <div className="space-y-4">
        <button
          onClick={() => handleMethodClick('pat')}
          className={`w-full p-6 border-2 rounded-lg transition-all text-left ${
            selectedMethod === 'pat'
              ? 'border-primary ring-2 ring-primary/20 bg-primary/5'
              : 'border-border hover:border-muted-foreground/50 hover:bg-accent'
          }`}
        >
          <div className="flex items-start space-x-4">
            <div className="flex-shrink-0 mt-1">
              <Key className="w-8 h-8" />
            </div>
            <div className="flex-1">
              <h3 className="font-semibold text-lg mb-1">
                Personal Access Token (PAT)
              </h3>
              <p className="text-sm text-muted-foreground mb-3">
                Quick setup using a personal access token. Best for personal
                projects and testing.
              </p>
              <div className="space-y-1">
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> Simple setup
                </p>
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> Works with private
                  repositories
                </p>
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> No public access required
                </p>
                <p className="text-xs text-orange-600 dark:text-orange-500 flex items-center">
                  <span className="mr-1">⚠</span> Limited to your personal
                  repositories
                </p>
              </div>
            </div>
          </div>
        </button>

        <button
          onClick={() => handleMethodClick('app')}
          className={`w-full p-6 border-2 rounded-lg transition-all text-left ${
            selectedMethod === 'app'
              ? 'border-primary ring-2 ring-primary/20 bg-primary/5'
              : 'border-border hover:border-muted-foreground/50 hover:bg-accent'
          }`}
        >
          <div className="flex items-start space-x-4">
            <div className="flex-shrink-0 mt-1">
              <Globe className="w-8 h-8" />
            </div>
            <div className="flex-1">
              <h3 className="font-semibold text-lg mb-1">GitHub App</h3>
              <p className="text-sm text-muted-foreground mb-3">
                Create a GitHub App for organization-wide access and advanced
                features.
              </p>
              <div className="space-y-1">
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> Organization-wide access
                </p>
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> Fine-grained permissions
                </p>
                <p className="text-xs text-green-600 dark:text-green-500 flex items-center">
                  <span className="mr-1">✓</span> Webhook support
                </p>
                <p className="text-xs text-orange-600 dark:text-orange-500 flex items-center">
                  <span className="mr-1">⚠</span> Requires public endpoint for
                  webhooks
                </p>
              </div>
            </div>
          </div>
        </button>
      </div>

      {onBack && (
        <div className="flex justify-start">
          <button
            onClick={onBack}
            className="px-4 py-2 text-muted-foreground hover:text-foreground transition-colors"
          >
            ← Back to provider selection
          </button>
        </div>
      )}
    </div>
  )
}
