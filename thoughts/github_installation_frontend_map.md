# Frontend GitHub App Installation & Setup - Complete Code Map

## Overview
The GitHub app installation and setup flow is spread across multiple components in the frontend, with no centralized landing page for post-installation callbacks. The application relies on polling and visibility detection to handle the GitHub app creation workflow.

---

## Key Frontend Files & Components

### 1. Dashboard & Entry Points
**File:** `/web/src/pages/Dashboard.tsx`
- Shows onboarding dashboard when no projects exist
- Displays `ImprovedOnboardingDashboard` component
- Entry point for GitHub setup wizard flow

### 2. Git Provider Management Pages

#### Git Providers List Page
**File:** `/web/src/pages/GitSources.tsx`
- Displays list of all git providers (GitHub, GitLab)
- Shows provider status (Active/Inactive)
- Has "Add Git Provider" button
- Includes:
  - `isGitHubApp()` helper to check if provider is GitHub App type
  - `handleInstallGitHubApp()` - Opens GitHub installation URL: `{baseUrl}/installations/new`
  - Delete provider with safety check
  - Feedback alerts for user feedback

**Key Code Locations:**
- Line 90-102: GitHub App installation handler
- Line 56-58: Helper to identify GitHub App providers

#### Add Git Provider Page
**File:** `/web/src/pages/AddGitProvider.tsx`
- Simple wrapper around `GitProviderFlow` component
- Shows success feedback after provider added
- Navigates back to `/git-sources` after success

#### Git Provider Detail Page
**File:** `/web/src/pages/GitProviderDetail.tsx` (THE MOST IMPORTANT FILE)
- **Displays detailed information about a single git provider**
- Shows GitHub App setup instructions when no connections exist (lines 401-432)
- Displays connections table with installation IDs
- Key handlers:
  - `handleInstallGitHubApp()` (lines 131-147) - Opens installation URL in new tab
  - Shows "Install GitHub App" button in multiple places:
    - Top right of page (lines 287-295)
    - In connections card header (lines 474-484)
    - In empty state when no connections (lines 514-522)
    - In GitHub App setup instructions card (lines 422-429)
- **GitHub App Setup Card** (lines 401-432):
  - Shows "Installation Required" message
  - Displays instructions for installing GitHub App
  - Only shown if provider is GitHub App AND no connections exist

**Key Code Sections:**
- Lines 55-62: Helper functions for provider type checking
- Lines 116-147: Authorization and installation handlers
- Lines 308-431: Provider details and setup instructions rendering

### 3. Main Git Provider Flow Component

**File:** `/web/src/components/git-providers/GitProviderFlow.tsx` (CRITICAL FILE)
- **Multi-step wizard for adding git providers**
- Handles GitHub App creation and installation flow
- **Polling mechanism** for detecting new installations (lines 98-101):
  ```typescript
  const { data: gitProviders = [], refetch: _refetchProviders } = useQuery({
    ...listGitProvidersOptions(),
    refetchInterval: isPollingInstallations ? 2000 : false, // Poll every 2s
  })
  ```

**Steps:**
1. **Provider Selection** (line 529): Choose GitHub or GitLab
2. **Authentication Method** (line 723): Choose between:
   - Create GitHub App (manifest-based)
   - Use Existing GitHub App (if already exists)
   - Personal Access Token (PAT)
3. **Configuration**: Different forms for each method
4. **Success Screen** (line 504)

**GitHub App Creation Flow:**
- Lines 267-374: `handleCreateGitHubAppManifest()`
  - Generates manifest with webhook URLs
  - Posts form to `https://github.com/settings/apps/new?state={source}`
  - Starts polling after 100ms timeout (lines 346-350)

**Polling & Installation Detection:**
- Lines 153-191: Detects when new GitHub App provider is created
- Uses `queueMicrotask()` to defer state updates
- Shows success toast when app detected (line 178)
- Polling times out after 60 seconds (lines 135-151)

**Webhook URLs:**
- Webhook: `{origin}/api/webhook/git/github/events`
- Callback: `{origin}/api/webhook/git/github/callback`
- Auth: `{origin}/api/webhook/git/github/auth`

### 4. Connections Table Component

**File:** `/web/src/components/git/ConnectionsTable.tsx`
- Displays list of git connections per provider
- Shows installation IDs for GitHub App connections
- Lines 161-169: Displays `installation_id` field:
  ```tsx
  {connection.installation_id ? (
    <span className="font-mono text-sm">
      {connection.installation_id}
    </span>
  ) : (
    <span className="text-muted-foreground">-</span>
  )}
  ```
- Can sync repositories per connection
- Can delete connections

---

## GitHub Installation Flow - Current Architecture

### 1. User Initiates Installation
```
/git-sources → AddGitProvider → GitProviderFlow
  ↓ (Select "Create GitHub App")
  ↓ (Enter manifest details & submit)
  ↓ GitHub App creation form submitted to GitHub
```

### 2. GitHub App Creation
- User redirected to: `https://github.com/settings/apps/new?state={source}`
- User fills in GitHub app settings
- User clicks "Create GitHub App"
- GitHub generates app_id and client_secret

### 3. Installation
- User clicks "Install" on the created app
- GitHub redirects to: `{origin}/api/webhook/git/github/callback`
- **Backend creates connection and provider record**
- Backend returns connection data with `installation_id`

### 4. Frontend Detection
- GitProviderFlow component is polling every 2 seconds (line 100)
- Detects when new GitHub App provider is created
- Shows success toast and navigates to success screen
- User can then view connections in GitProviderDetail page

---

## Query Parameters & Callbacks Handled

### Current Implementation
**NO dedicated callback page exists.** The flow handles post-installation via:

1. **Polling**: GitProviderFlow polls for new providers every 2 seconds
2. **Visibility Detection**: GitHubAppCreationForm detects tab visibility change (lines 46-75)
3. **Manual Refresh**: User can manually refresh GitProviderDetail page

### GitHub Callback URLs (Backend)
- `/api/webhook/git/github/events` - Webhook events endpoint
- `/api/webhook/git/github/callback` - Installation callback
- `/api/webhook/git/github/auth` - OAuth authorization endpoint

**These are backend endpoints, not frontend pages.**

---

## User Feedback & Status Display

### Success Messages
- **GitProviderFlow**: Toast notifications via `sonner`
  - "GitHub App created successfully!" (line 178)
  - "Opening GitHub App installation in new tab" (line 145, 345)
  - "Watching for new installations..." (line 348-350)

### Loading States
- Loading spinners in GitProviderFlow and GitProviderDetail
- "Verifying..." state during sync operations
- Animation spinners during async operations

### Status Display in UI

#### In GitProviderDetail Page
- **Header Badge**: Shows "Active" or "Inactive" status (lines 259-275)
- **Provider Information Card**: Shows auth method, base URL, timestamps
- **GitHub App Setup Card**: Shows instructions when no connections (lines 401-432)
- **Connections Table**: Shows all connections with:
  - Connection name
  - Repository name
  - Installation ID (if GitHub App)
  - Sync/authorize buttons
  - Last sync time

---

## Key React Hooks & Patterns Used

### useQuery
- Lists git providers
- Lists connections
- Gets provider details
- Auto-refetch/polling capability

### useMutation
- Create GitHub/GitLab PAT provider
- Create GitLab OAuth provider
- Sync repositories
- Delete connections

### Custom Hooks
- `useFeedback()` - Manages feedback alerts
- `useBreadcrumbs()` - Breadcrumb navigation
- `usePageTitle()` - Page title management
- `useKeyboardShortcut()` - Keyboard shortcuts (e.g., "N" for add)

### State Management
- Local component state for form inputs
- QueryClient for data caching
- Polling for installation detection
- Visibility detection for tab changes

---

## Missing or Incomplete Features

### 1. No Dedicated Installation Complete Page
- Currently uses polling in GitProviderFlow
- Could benefit from a dedicated callback page that:
  - Shows installation success message
  - Displays created provider details
  - Allows next steps (like creating connections)
  - Has a "View Provider" button to navigate to GitProviderDetail

### 2. No Query Parameter Handling for Callbacks
- GitHub redirects to `/api/webhook/git/github/callback`
- Frontend doesn't handle state/code parameters from GitHub
- No way to directly navigate to success page after installation
- All detection happens via polling

### 3. Setup URL Not Implemented
- GitHub app manifest includes `setup_url` field (line 308 in GitProviderFlow)
- Could be used to show post-installation instructions
- Currently not being leveraged

### 4. No Persistent Installation Status
- If user closes tab during polling, installation detection stops
- No webhook or socket-based real-time updates
- User must manually refresh or revisit page

---

## Component Hierarchy

```
Dashboard
├── ImprovedOnboardingDashboard
│   └── (May include GitHub setup wizard)
├── GitSources (List Page)
│   └── Provider cards with Install buttons
├── GitProviderDetail (Detail Page) ← Shows GitHub App setup & connections
│   ├── GitHub App Setup Card (when no connections)
│   ├── ConnectionsTable
│   └── Sync/Authorize buttons
├── AddGitProvider (Add Page)
│   └── GitProviderFlow (Multi-step wizard) ← Main GitHub app flow
│       ├── Provider Selection
│       ├── Method Selection
│       ├── GitHub App Creation Manifest
│       ├── GitHub App Installation Handler
│       └── Success Screen
```

---

## File Locations Summary

| Purpose | File Path | Key Lines |
|---------|-----------|-----------|
| Dashboard entry | `/web/src/pages/Dashboard.tsx` | - |
| Git providers list | `/web/src/pages/GitSources.tsx` | 90-102 (install handler) |
| Add provider flow | `/web/src/pages/AddGitProvider.tsx` | - |
| Provider details & setup | `/web/src/pages/GitProviderDetail.tsx` | 131-147, 401-432 |
| Multi-step wizard | `/web/src/components/git-providers/GitProviderFlow.tsx` | 98-101 (polling), 267-374 (creation), 153-191 (detection) |
| Connections display | `/web/src/components/git/ConnectionsTable.tsx` | 161-169 (installation_id) |
| GitHub app form | `/web/src/components/onboarding/GitHubAppCreationForm.tsx` | 46-75 (visibility detection) |

---

## Backend Integration Points

### API Endpoints Called
- **GET** `/git-providers` - List all providers (GitProviderFlow line 99)
- **POST** `/git-providers/github/pat` - Create GitHub PAT provider
- **POST** `/git-providers/gitlab/pat` - Create GitLab PAT provider
- **POST** `/git-providers/gitlab/oauth` - Create GitLab OAuth provider
- **GET** `/git-providers/{id}` - Get provider details
- **GET** `/git/connections` - List connections
- **POST** `/git/connections/{id}/sync` - Sync repositories
- **DELETE** `/git/providers/{id}` - Delete provider

### GitHub OAuth Flow
1. Frontend posts manifest to GitHub: `https://github.com/settings/apps/new`
2. GitHub creates app and redirects to: `{origin}/api/webhook/git/github/callback`
3. Backend creates provider and connection records
4. Frontend polls `/git-providers` to detect new provider

---

## Recommendations for Improvement

### 1. Add Installation Success Page
Create `/web/src/pages/GitHubInstallationComplete.tsx`
- Show installation success message
- Display created provider details
- Link to GitProviderDetail page
- Handle state verification

### 2. Implement Setup URL
- Use GitHub app `setup_url` field for post-installation redirect
- Redirect to dedicated success page with provider context

### 3. Improve Installation Detection
- Use WebSocket or server-sent events instead of polling
- Show real-time progress updates
- Reduce polling frequency to save resources

### 4. Better Error Handling
- Show specific error messages for installation failures
- Provide troubleshooting steps
- Allow retry mechanisms

### 5. Installation Status Badge
- Add badge to Git Sources list showing "Installation Complete" status
- Show warning if app is created but not installed
