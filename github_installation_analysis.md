# GitHub App Installation Processing - Code Analysis Report

## Overview
Analysis of GitHub app installation redirect handling in the Temps codebase. The code handles both GitHub App manifest conversion and OAuth installation flows.

---

## 1. Installation Redirect Endpoints

### Primary Endpoints (in `/crates/temps-git/src/handlers/github.rs`)

#### 1.1 OAuth Authorization Callback - `github_app_auth_callback`
- **Route**: `GET /webhook/git/github/auth`
- **Location**: Lines 418-539 in `github.rs`
- **Purpose**: Handles both GitHub App manifest conversion AND OAuth installation flow
- **Parameters**:
  - `code`: OAuth authorization code (required)
  - `state`: OAuth state parameter
  - `installation_id`: GitHub installation ID (optional, for OAuth flow)
  - `setup_action`: Action type (install, request, or update)

**Key Flow:**
1. If `installation_id` is present → OAuth installation flow (process installation directly)
2. If only `code` + `state` → Manifest conversion flow (exchange code for GitHub App)
3. Otherwise → Auth-only flow (wait for webhook)

#### 1.2 Installation Callback - `github_app_installation_callback`
- **Route**: `GET /webhook/git/github/callback`
- **Location**: Lines 541-643 in `github.rs`
- **Purpose**: Fallback/alternative installation callback (for backward compatibility)
- **Parameters**:
  - `installation_id`: GitHub installation ID (required)
  - `code`: Authorization code (optional)
  - `setup_action`: Action type

---

## 2. Installation Processing Logic

### Core Function: `process_installation()`
- **Location**: `/crates/temps-git/src/services/github.rs`, lines 1091-1197
- **Parameters**:
  - `installation_id_p: i32` - The GitHub installation ID
  - `app_id: Option<i32>` - Optional GitHub App ID for direct lookup

### Processing Flow

```
process_installation()
  ↓
1. Identify which GitHub App owns the installation
   - If app_id provided: use it directly
   - Else: try each GitHub App provider until one can access installation
  ↓
2. Call create_github_app_installation()
   ↓
   a. Get installation details from GitHub API
   b. Create installation access token
   c. Create git_provider_connections entry
   d. Sync repositories
   ↓
3. Return connection details
```

### Detailed Steps in `create_github_app_installation()`

**Location**: `/crates/temps-git/src/services/github.rs`, lines 744-804

Steps:
1. **Get GitHub App credentials** (Octocrab client + app data)
2. **Fetch installation from GitHub API** using InstallationId
3. **Create installation access token** via POST to access_tokens_url
4. **Parse token expiration** from GitHub API response
5. **Create connection in database**:
   - Calls `git_provider_manager.create_connection()`
   - Encrypts access token
   - Stores with metadata (account_id, repository_selection, html_url, etc.)
6. **Sync repositories**
   - Fetches all repos the installation has access to
   - Stores/updates in database
   - Queues framework detection jobs

---

## 3. Database Layer

### Relevant Entities

#### `git_providers`
Stores GitHub App definition (credentials, webhook secret, etc.)
- Linked to `git_provider_connections` via `provider_id`

#### `git_provider_connections`
Stores individual installations/OAuth connections
- **Key fields for installations**:
  - `installation_id: Option<String>` - GitHub installation ID (stored as string)
  - `provider_id: i32` - FK to git_providers
  - `access_token: Option<String>` - Encrypted installation token
  - `token_expires_at: Option<DateTime>` - Token expiration
  - `is_active: bool` - Connection status
  - `metadata: Option<JSON>` - Account details, repository selection, etc.

### Connection Lookup
- **Location**: `git_provider_manager.get_connection_by_installation_id()`
- **Query**: Finds active connection by `installation_id`
- Returns error if not found (does NOT create if missing)

---

## 4. Error Handling

### Error Points in Installation Processing

#### Handler Level (github.rs)
```rust
// Lines 505-513 (auth_callback)
Err(e) => {
    error!("Failed to process installation: {:?}", e);
    return Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Installation Processing Failed")
        .with_detail(format!(
            "Failed to process installation {}: {}",
            installation_id, e
        )));
}
```

#### Webhook Event Handler (lines 359-363)
```rust
Err(e) => {
    error!(
        "Failed to process installation {} via webhook: {:?}",
        installation_id, e
    );
}
// Note: Error is logged but NOT returned (handler continues)
```

### Error Types (`GithubAppServiceError`)
- `NotFound(String)` - App/installation not found
- `GithubApiError(String)` - GitHub API call failed
- `DatabaseError(DbErr)` - Database operation failed
- `EncryptionFailed(String)` - Token encryption failed
- `Conflict(String)` - App already exists
- `Other(String)` - Generic errors

---

## 5. Duplicate Processing Issues

### **CRITICAL FINDING: No Idempotency Check**

The code has **NO idempotency protection**. If `process_installation()` is called twice with the same `installation_id`:

1. **First call**:
   - Fetches installation from GitHub API ✓
   - Creates access token ✓
   - **Calls `git_provider_manager.create_connection()`** - Success
   - Syncs repositories ✓

2. **Second call** (if triggered again):
   - Fetches installation from GitHub API again ✓
   - Creates NEW access token (old one still valid) ✓
   - **Calls `git_provider_manager.create_connection()` AGAIN** - DUPLICATE CREATED
   - Attempts to sync repositories again
   - **Database constraint violation likely** (depending on schema)

### Why Duplicates Can Occur

1. **Webhook Event + User Action**:
   - Webhook sends installation event → `process_installation()` called
   - User manually visits `/webhook/git/github/auth?installation_id=X` → `process_installation()` called again

2. **Network Retry**:
   - Initial request hangs, user clicks "Try Again"
   - Both requests process the same installation

3. **Multiple GitHub Apps**:
   - If app_id is not provided, code tries all GitHub Apps
   - Could theoretically match same installation with different apps

### Current Safeguards (Insufficient)

**Lookup method exists but NOT called before create**:
```rust
// In git_provider_manager (line 2689)
pub async fn get_connection_by_installation_id(&self, installation_id: &str) -> Result<...>
// This method EXISTS but is NEVER called in process_installation()
```

The method is only called in:
- `update_last_synced_at()`
- `get_repositories_for_installation()`
- But NOT in `process_installation()` before creating connection

---

## 6. Repository Synchronization

### Location: `/crates/temps-git/src/services/github.rs`, lines 628-742

### `sync_repositories()` Process
1. Fetches all repos from `/installation/repositories` API
2. Uses transaction to update/insert repos
3. Updates `last_synced_at` timestamp
4. Queues framework detection jobs for each repo

### Idempotency in Repository Sync
- Uses `save()` which does upsert (insert or update)
- **Safe for re-running** - won't create duplicates
- Looks up by `full_name` to determine update vs insert

---

## 7. Webhook Event Handling

### Installation Event Handler
**Location**: Lines 336-382 in `github.rs` - `handle_installation_event()`

Handles webhook events:
- `created` - Calls `process_installation()`
- `deleted` - Calls `delete_installation()`

**Issue**: No deduplication of webhook events
- If GitHub sends same webhook twice → code runs twice
- GitHub typically deduplicates, but best practice is idempotent handlers

### Installation Repositories Event Handler
**Location**: Lines 296-334 - `handle_installation_repositories()`

Handles `added` action:
- Syncs each added repository
- Uses `sync_repository()` which is idempotent

---

## 8. File Locations Summary

| Component | File | Lines |
|-----------|------|-------|
| Handler Endpoints | `crates/temps-git/src/handlers/github.rs` | 24-43 (routes), 418-643 (handlers) |
| Service - Installation Processing | `crates/temps-git/src/services/github.rs` | 1091-1197 |
| Service - Create Installation | `crates/temps-git/src/services/github.rs` | 744-804 |
| Service - Sync Repositories | `crates/temps-git/src/services/github.rs` | 628-742 |
| Database Manager - Create Connection | `crates/temps-git/src/services/git_provider_manager.rs` | 635-676 |
| Database Manager - Lookup Connection | `crates/temps-git/src/services/git_provider_manager.rs` | 2689-2709 |
| Webhook Events | `crates/temps-git/src/handlers/github.rs` | 156-246 (main), 336-382 (installation) |

---

## 9. Recommendations for Duplicate Prevention

### Option A: Pre-Check Before Create (Recommended)
```rust
// In process_installation(), before create_github_app_installation():
match self.git_provider_manager
    .get_connection_by_installation_id(&installation_id_p.to_string())
    .await
{
    Ok(existing) => {
        // Connection already exists - update/refresh if needed
        info!("Installation {} already exists, refreshing...", installation_id_p);
        // Could update token, resync, etc.
        return Ok(existing);
    }
    Err(_) => {
        // Connection doesn't exist, proceed with creation
        let connection = self.create_github_app_installation(...).await?;
    }
}
```

### Option B: Database Unique Constraint
Add unique constraint on `git_provider_connections(provider_id, installation_id)`:
- Prevents duplicate inserts at database level
- Returns meaningful error for retry logic

### Option C: Idempotent Token Generation
Store token generation timestamp, skip if recent:
- Only refresh token if older than X seconds
- Reduces GitHub API calls
- Naturally handles retries

---

## 10. Summary of Findings

### Critical Issues
1. **NO idempotency check** before creating connection
2. **Duplicate webhook events** not deduplicated
3. **create_connection()** always inserts new record
4. **get_connection_by_installation_id()** exists but not used

### Safe Operations
- Repository sync is idempotent (uses upsert)
- Framework detection is queued safely
- Error handling returns appropriate HTTP status codes

### Recommended Action
Implement pre-check before `create_github_app_installation()` to:
1. Check if installation already exists
2. Handle update case (token refresh)
3. Prevent duplicate connections
