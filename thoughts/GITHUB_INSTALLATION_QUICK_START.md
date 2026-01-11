# GitHub Installation Frontend - Quick Start Guide

## TL;DR - Where to Find Everything

### Main Files You Need to Know
1. **`/web/src/pages/GitProviderDetail.tsx`** - WHERE INSTALLATION STATUS IS SHOWN
   - Lines 131-147: Installation handler
   - Lines 401-432: GitHub App setup instructions card
   - Lines 287-295, 474-484, 514-522, 422-429: Install buttons in multiple places

2. **`/web/src/components/git-providers/GitProviderFlow.tsx`** - THE WIZARD & POLLING LOGIC
   - Lines 98-101: Polling mechanism
   - Lines 267-374: GitHub app creation
   - Lines 153-191: Installation detection

3. **`/web/src/pages/GitSources.tsx`** - PROVIDER LIST PAGE
   - Lines 90-102: Install app handler

4. **`/web/src/components/git/ConnectionsTable.tsx`** - DISPLAYS INSTALLATION_ID
   - Lines 161-169: Shows installation_id field

### Key Flows

#### User Installs GitHub App
```
Dashboard/GitSources → AddGitProvider → GitProviderFlow
  ↓ (Select GitHub → Create App)
  ↓ Posts manifest to GitHub
  ↓ Starts polling every 2 seconds
  ↓ User installs app at GitHub
  ↓ GitHub calls /api/webhook/git/github/callback (BACKEND)
  ↓ Backend creates provider & connection with installation_id
  ↓ Polling detects new provider
  ↓ Shows success toast
  ↓ Success screen → Navigate to detail page or list
```

#### User Sees Installation Status
```
GitProviderDetail page
  ↓ (Provider type = GitHub App)
  ↓ If no connections exist:
  │  • Shows "GitHub App Setup Card" with installation instructions
  │  • Shows "Install GitHub App" button
  ↓ Connections Table:
    • Lists all connections
    • Shows installation_id in monospace font
    • installation_id = "12345678" (from GitHub)
```

---

## What Installation_ID Is

The `installation_id` is a unique numeric ID that GitHub generates when you install a GitHub App.

**Journey:**
1. User creates GitHub App at GitHub settings
2. User clicks "Install" on the GitHub app
3. GitHub redirects to: `/api/webhook/git/github/callback?installation_id=12345678&code=...&state=...`
4. Backend receives installation_id and stores in database
5. Frontend queries `/git/connections` and gets back `installation_id` in response
6. Displayed in ConnectionsTable as text

**Storage:** `ConnectionResponse.installation_id` field
**Display:** ConnectionsTable.tsx lines 161-169

---

## Installation Status Displays

### In GitProviderDetail.tsx
1. **Top Right Button** (lines 287-295)
   - "Install GitHub App" button
   - Only shown if provider is GitHub App type

2. **Provider Information Card** (lines 318-374)
   - Shows auth method: "GitHub App"
   - Shows status: "Active" or "Inactive"

3. **GitHub App Setup Card** (lines 401-432)
   - Only shown if:
     - Provider is GitHub App type AND
     - No connections exist
   - Contains:
     - "Installation Required" heading
     - Instructions text
     - "Install GitHub App" button

4. **Connections Table Header** (lines 474-484)
   - "Install GitHub App" button (if GitHub App type)

5. **Empty State** (lines 514-522)
   - "Install GitHub App" button (if GitHub App type and no connections)

6. **Connections Table Body** (lines 161-169 in ConnectionsTable.tsx)
   - Shows installation_id in monospace font
   - Only for GitHub App connections

---

## How Polling Works

**File:** `/web/src/components/git-providers/GitProviderFlow.tsx`

### Setup (lines 98-101)
```typescript
const { data: gitProviders = [], refetch } = useQuery({
  ...listGitProvidersOptions(),
  refetchInterval: isPollingInstallations ? 2000 : false
})
```

### Start Polling
- User clicks "Create GitHub App"
- Posts manifest form to GitHub
- Sets `isPollingInstallations = true`
- Query auto-refetches every 2 seconds

### Detection (lines 153-191)
```typescript
if (isPollingInstallations && currentCount > previousCount) {
  // New provider detected!
  // Show success toast
  // Stop polling
  // Show success screen
}
```

### Timeout (lines 135-151)
- Polls for max 60 seconds
- Shows info toast if timeout
- User can manually refresh

---

## Quick Test Checklist

✅ Can I see GitHub App setup instructions?
   → Go to GitProviderDetail page for GitHub App provider with no connections
   → Should see "Installation Required" card

✅ Can I see the installation button?
   → Should be in multiple places:
     • Top right of page
     • In setup instructions card
     • In connections table header
     • In empty state

✅ Does the button open installation?
   → Clicks should open GitHub installation page in new tab
   → URL: `{baseUrl}/installations/new`

✅ Can I see installation_id after installing?
   → Connections table should show installation_id
   → Usually a 7-8 digit number like "12345678"

✅ Does polling work?
   → Install GitHub App in GitHub
   → Should see success toast in GitProviderFlow
   → Should see new connection appear

---

## Common Issues & Solutions

**Issue:** Installation button doesn't appear
- **Solution:** Check if provider type is "github" AND auth_method is "github_app" or "app"
  - Helper function: `isGitHubApp()` (line 56)

**Issue:** Polling doesn't detect installation
- **Solution:**
  - Check if backend is creating provider correctly
  - Check if `listGitProviders` API is returning new provider
  - Polling waits up to 60 seconds

**Issue:** Installation_id not showing in table
- **Solution:**
  - Refresh page or wait for query to update
  - Check if connection has `installation_id` field populated
  - Some connection types (PAT) won't have installation_id

**Issue:** User closes tab during installation
- **Solution:**
  - Polling stops when component unmounts
  - User can manually refresh GitProviderDetail page
  - Or return to GitSources and re-enter detail page

---

## Important Hooks & Patterns

### Feedback
```typescript
const { feedback, showSuccess, clearFeedback } = useFeedback()
// Shows alerts at top of page
```

### Query
```typescript
const { data: provider, isLoading } = useQuery({
  ...getGitProviderOptions({ path: { provider_id } })
})
```

### Toast Notifications
```typescript
import { toast } from 'sonner'
toast.success('Installation complete')
toast.error('Installation failed')
toast.info('Watching for installations...')
```

---

## Backend Integration Points

**Endpoints Called:**
- `GET /git-providers` - List all providers (polling target)
- `GET /git-providers/{id}` - Get single provider
- `GET /git/connections` - Get connections with installation_id
- `POST /git/connections/{id}/sync` - Sync repositories

**Callback Handled by Backend:**
- `GET /api/webhook/git/github/callback?installation_id=...&code=...`
  - Frontend doesn't directly handle this
  - Backend processes it and creates provider/connection

**GitHub App Manifest Sent To:**
- `https://github.com/settings/apps/new`
  - Frontend creates form and POSTs manifest
  - User completes setup at GitHub

---

## Files You Might Need to Modify

### If You Want to...

**Add installation success page:**
- Create: `/web/src/pages/GitHubInstallationComplete.tsx`
- Update: App.tsx routing
- Update: GitProviderFlow.tsx onSuccess callback

**Show installation status on list page:**
- Edit: `/web/src/pages/GitSources.tsx`
- Add badge or indicator for "Installed" vs "Created" status

**Improve polling detection:**
- Edit: `/web/src/components/git-providers/GitProviderFlow.tsx` lines 98-191
- Could implement WebSocket/SSE instead

**Customize installation instructions:**
- Edit: `/web/src/pages/GitProviderDetail.tsx` lines 401-432
- Modify GitHub App Setup Card content

**Change installation URL:**
- Edit: `GitProviderDetail.tsx` lines 131-147 `handleInstallGitHubApp()`
- Edit: `GitSources.tsx` lines 90-102 `handleInstallGitHubApp()`
- Currently: `{baseUrl}/installations/new`

---

## Related Documentation Files

- `github_installation_frontend_map.md` - Full technical breakdown
- `github_installation_flow_diagram.txt` - ASCII diagrams and flows
