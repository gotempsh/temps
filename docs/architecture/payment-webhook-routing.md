# Payment Provider Webhook Routing Architecture

## Status
**Proposed** - Not yet implemented

## Overview

This document describes the architecture for a unified webhook routing system that allows multiple Temps projects to share a single payment provider account (Stripe, Lemon Squeezy, etc.) while automatically routing webhooks to the correct project and environment.

## Problem Statement

### Current State (Manual Configuration)
Users must currently configure separate webhooks for each environment:
- **Production**: `https://prod.example.com/stripe/webhook`
- **Staging**: `https://staging.example.com/stripe/webhook`
- **Preview**: `https://preview-pr-123.example.com/stripe/webhook`

**Pain Points:**
1. 30-45 minutes setup time per project (10-15 min × 3 environments)
2. 3-5 different webhook URLs to manage
3. Manual price ID mapping required
4. High risk of misconfiguration
5. Difficult to debug routing issues
6. New prices require manual reconfiguration

### Desired State (Automatic Routing)
A single global webhook endpoint that automatically routes to the correct environment:
- **All webhooks**: `https://temps.io/api/_temps/stripe/webhook`

**Benefits:**
1. 5 minutes total setup time
2. 1 webhook URL for all projects
3. Automatic routing via price ID filters
4. Low risk of errors (UI-guided setup)
5. Clear visibility via event log
6. New prices work immediately after UI configuration

## Design Goals

### Must Have
1. **Single Global Endpoint**: One webhook URL for all projects on a Temps instance
2. **Multi-Project Support**: Multiple projects can share the same Stripe account
3. **Automatic Routing**: Route webhooks based on price_id without manual mapping
4. **UI-Only Configuration**: No .temps.yaml changes required (separates code from config)
5. **Security**: Webhook signature verification per project
6. **Visibility**: Event log showing all webhook activity and routing decisions

### Should Have
1. **Fallback Handling**: Queue unrouted webhooks for manual assignment
2. **Multiple Providers**: Support Stripe, Lemon Squeezy, Paddle
3. **Audit Trail**: Track who configured which price IDs
4. **Error Recovery**: Retry failed webhooks

### Nice to Have
1. **Advanced Filters**: Route by customer_id, subscription_id, metadata
2. **Conditional Routing**: Route based on webhook event type
3. **Webhook Testing**: Send test events from UI

## Architecture

### High-Level Flow

```
Payment Provider (Stripe)
        ↓
Single Global Webhook URL
  https://temps.io/api/_temps/stripe/webhook
        ↓
Extract: account_id, price_id, livemode from payload
        ↓
Lookup: Which project owns this Stripe account?
        ↓
Verify: Webhook signature with project's secret
        ↓
Lookup: Which environment has routing rule for this price_id?
        ↓
Route: Forward webhook to environment's handler
        ↓
Log: Record event in webhook_events table
```

### Components

#### 1. Webhook Handler (HTTP Layer)
- **Location**: `crates/temps-webhooks/src/handlers/`
- **Responsibility**: Receive webhooks, verify signatures, trigger routing
- **Endpoints**:
  - `POST /api/_temps/stripe/webhook`
  - `POST /api/_temps/lemon-squeezy/webhook`
  - `POST /api/_temps/paddle/webhook`

#### 2. Routing Service (Business Logic)
- **Location**: `crates/temps-webhooks/src/services/routing_service.rs`
- **Responsibility**:
  - Extract identifiers from webhook payload (price_id, account_id, etc.)
  - Lookup routing rules in database
  - Verify webhook signatures
  - Handle unrouted webhooks (no matching rule)
  - Log all events

#### 3. Webhook Processor (Environment Integration)
- **Location**: `crates/temps-webhooks/src/services/webhook_processor.rs`
- **Responsibility**: Forward verified webhook to environment's application
- **Methods**:
  - HTTP proxy to environment URL
  - Message queue (for async processing)
  - Database event store (for polling)

#### 4. Configuration UI
- **Location**: `web/src/components/integrations/`
- **Responsibility**: Allow users to configure webhook routing via UI
- **Features**:
  - Stripe account setup wizard
  - Price ID management per environment
  - Webhook event log viewer
  - Unrouted webhook assignment

## Handling General Webhooks (Non-Transactional Events)

### Problem: Events Without Price IDs

Not all Stripe webhooks contain price information. Many events are **general/non-transactional** and cannot be routed based on price_id:

**Examples:**
- `customer.created`
- `customer.updated`
- `customer.deleted`
- `payment_method.attached`
- `payment_method.detached`
- `account.updated`
- `balance.available`
- `charge.dispute.created`

**Challenge**: How do we route these webhooks when there's no price_id to match against?

### Routing Strategies for General Webhooks

#### Strategy 1: Default Environment by Mode (Recommended for MVP)

Route based solely on `livemode` flag:
- **Live mode** (`livemode: true`) → Production environment
- **Test mode** (`livemode: false`) → Staging environment (highest priority)

**Pros:**
- Simple to implement
- Works for 90% of use cases
- No additional configuration needed

**Cons:**
- All general webhooks go to one environment per mode
- Can't route different event types to different environments

**Implementation:**
```rust
pub async fn route_general_webhook(
    &self,
    event: &stripe::Event,
    project_id: i32,
) -> Result<WebhookRoute, RoutingError> {
    // Route based on livemode flag only
    let environment = if event.livemode {
        // Live mode → Production
        environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::IsProduction.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or(RoutingError::NoProductionEnvironment)?
    } else {
        // Test mode → Highest priority non-production
        environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::IsProduction.eq(false))
            .order_by_desc(environments::Column::Priority)
            .one(self.db.as_ref())
            .await?
            .ok_or(RoutingError::NoStagingEnvironment)?
    };

    Ok(WebhookRoute {
        project_id,
        environment_id: environment.id,
        event: event.clone(),
        routing_method: "default_by_mode",
    })
}
```

#### Strategy 2: Event Type Routing Rules (Phase 2)

Allow users to configure routing rules for specific event types.

**Database Schema Addition:**
```sql
-- Add to webhook_routing_rules table
ALTER TABLE webhook_routing_rules
ADD COLUMN event_type_pattern VARCHAR(100);  -- e.g., 'customer.*', 'payment_method.*'

-- Example rules:
INSERT INTO webhook_routing_rules
  (project_id, environment_id, provider_type, filter_type, filter_value)
VALUES
  (12, 5, 'stripe', 'event_type', 'customer.*'),      -- All customer events
  (12, 5, 'stripe', 'event_type', 'charge.dispute.*'); -- All dispute events
```

**UI Configuration:**
```
┌─────────────────────────────────────────────────────────────────┐
│  General Webhook Routing                                        │
├─────────────────────────────────────────────────────────────────┤
│  Configure routing for non-transactional events                │
│                                                                 │
│  📦 Production Environment                                      │
│  ┌───────────────────────────────────────────────────────────┐ │
│  │ Event Type Patterns:                                      │ │
│  │ • customer.*                                   [Remove]   │ │
│  │ • charge.dispute.*                             [Remove]   │ │
│  │                                                           │ │
│  │ [customer.updated                         ] [Add Pattern]│ │
│  └───────────────────────────────────────────────────────────┘ │
│                                                                 │
│  📦 Staging Environment                                         │
│  ┌───────────────────────────────────────────────────────────┐ │
│  │ Event Type Patterns:                                      │ │
│  │ • payment_method.*                             [Remove]   │ │
│  └───────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

**Implementation:**
```rust
pub async fn route_by_event_type(
    &self,
    event: &stripe::Event,
    project_id: i32,
) -> Result<WebhookRoute, RoutingError> {
    let event_type = event.type_.as_str();

    // Find matching event type rule (supports wildcards)
    let rule = webhook_routing_rules::Entity::find()
        .filter(webhook_routing_rules::Column::ProjectId.eq(project_id))
        .filter(webhook_routing_rules::Column::FilterType.eq("event_type"))
        .all(self.db.as_ref())
        .await?
        .into_iter()
        .find(|r| {
            // Check if event type matches pattern (e.g., "customer.*" matches "customer.created")
            let pattern = &r.filter_value;
            if pattern.ends_with(".*") {
                let prefix = &pattern[..pattern.len() - 2];
                event_type.starts_with(prefix)
            } else {
                event_type == pattern
            }
        })
        .ok_or_else(|| RoutingError::NoEventTypeRule(event_type.to_string()))?;

    Ok(WebhookRoute {
        project_id,
        environment_id: rule.environment_id,
        event: event.clone(),
        routing_method: "event_type_pattern",
    })
}
```

#### Strategy 3: Broadcast to All Environments (Optional)

Send general webhooks to **all environments** simultaneously.

**Use Case**: Events like `account.updated` that all environments need to be aware of.

**Pros:**
- Ensures all environments stay in sync
- No configuration needed

**Cons:**
- More webhook traffic
- Potential duplicate processing

**Implementation:**
```rust
pub async fn broadcast_webhook(
    &self,
    event: &stripe::Event,
    project_id: i32,
) -> Result<Vec<WebhookRoute>, RoutingError> {
    // Get all environments for project
    let environments = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project_id))
        .all(self.db.as_ref())
        .await?;

    // Route to all environments
    let routes = environments
        .into_iter()
        .map(|env| WebhookRoute {
            project_id,
            environment_id: env.id,
            event: event.clone(),
            routing_method: "broadcast",
        })
        .collect();

    Ok(routes)
}
```

#### Strategy 4: Customer Metadata Routing (Advanced)

Use Stripe customer metadata to determine routing.

**Setup**: When creating customers, add metadata:
```javascript
// User's app code
const customer = await stripe.customers.create({
  email: 'user@example.com',
  metadata: {
    temps_environment_id: '5',  // Route to environment 5
    temps_project_id: '12',
  }
});
```

**Routing:**
```rust
pub async fn route_by_customer_metadata(
    &self,
    event: &stripe::Event,
    project_id: i32,
) -> Result<WebhookRoute, RoutingError> {
    // Extract customer ID from event
    let customer_id = self.extract_customer_id(event)?;

    // Fetch customer from Stripe API to get metadata
    // (Requires storing Stripe API keys, not just webhook secrets)
    let customer = self.stripe_client
        .get_customer(customer_id)
        .await?;

    // Extract environment_id from metadata
    let environment_id: i32 = customer
        .metadata
        .get("temps_environment_id")
        .and_then(|v| v.parse().ok())
        .ok_or(RoutingError::NoMetadataRouting)?;

    Ok(WebhookRoute {
        project_id,
        environment_id,
        event: event.clone(),
        routing_method: "customer_metadata",
    })
}
```

**Note**: Requires storing Stripe API keys (not recommended for MVP - webhook secrets only).

### Critical Insight: Multi-Project Broadcasting

**Problem**: General webhooks don't have project-specific identifiers.

**Scenario**: Two projects share the same Stripe account:
- **Project A**: E-commerce site with logic for `customer.updated`
- **Project B**: Subscription service with logic for `customer.updated`

When `customer.updated` arrives, **both projects need to receive it** because:
1. We can identify the Stripe account (`acct_123`)
2. But we **cannot** determine which project the customer belongs to
3. The customer could be relevant to both projects

**Solution**: Broadcast general webhooks to **all projects** using that Stripe account.

### Recommended Approach: Hybrid Routing with Broadcasting

**For Transactional Events (with price_id)**:
- Route to specific project/environment based on price_id mapping
- One webhook → One destination

**For General Events (without price_id)**:
- Broadcast to **all projects** using the Stripe account
- Within each project, route by mode (live → prod, test → staging)
- One webhook → Multiple destinations

**Example Flow**:
```
customer.updated webhook arrives
  ↓
Stripe account: acct_123
  ↓
Find all projects using acct_123:
  - Project A (id: 12)
  - Project B (id: 15)
  ↓
For each project:
  - If livemode: true → Route to Production env
  - If livemode: false → Route to Staging env
  ↓
Result:
  - Project A → Production (env_id: 5)
  - Project B → Production (env_id: 8)
```

**Phase 1 (MVP)**: Broadcast general webhooks
- All projects using same Stripe account receive general webhooks
- Route by livemode within each project
- Simple, handles all edge cases

**Phase 2**: Add event type filtering (optional)
- Projects can opt-in to specific event types
- Example: Project A only wants `customer.*` events
- Projects that don't configure filters receive all general webhooks

**Phase 3+**: Add customer metadata routing (advanced)
- Requires Stripe API keys
- Can route specific customers to specific projects

### Complete Routing Logic with Broadcasting

```rust
impl WebhookRoutingService {
    pub async fn route_stripe_webhook(
        &self,
        raw_body: &str,
        signature: &str,
    ) -> Result<Vec<WebhookRoute>, RoutingError> {
        // 1. Parse webhook
        let event: stripe::Event = serde_json::from_str(raw_body)?;
        let account_id = event.account.as_ref().ok_or(RoutingError::MissingAccountId)?;

        // 2. Get ALL projects using this Stripe account
        let webhook_configs = stripe_webhook_configs::Entity::find()
            .filter(stripe_webhook_configs::Column::StripeAccountId.eq(account_id))
            .all(self.db.as_ref())
            .await?;

        if webhook_configs.is_empty() {
            return Err(RoutingError::UnknownAccount(account_id.to_string()));
        }

        // 3. Verify signature (use first project's secret - all share same account)
        let webhook_secret = if event.livemode {
            self.encryption.decrypt(&webhook_configs[0].webhook_secret_live)?
        } else {
            self.encryption.decrypt(&webhook_configs[0].webhook_secret_test)?
        };

        stripe::Webhook::construct_event(raw_body, signature, &webhook_secret)
            .map_err(|_| RoutingError::InvalidSignature)?;

        // 4. Determine routing strategy based on event type
        let mut routes = Vec::new();

        if self.has_price_id(&event) {
            // Strategy A: Route by price_id (transactional events)
            // One webhook → One specific project/environment
            let price_id = self.extract_price_id(&event)?;

            let route = self.route_by_price_id(&event, price_id).await?;
            routes.push(route);
        } else {
            // Strategy B: Broadcast general webhooks
            // One webhook → Multiple projects/environments
            for config in webhook_configs {
                let route = if self.has_event_type_rule(&event, config.project_id).await? {
                    // Route by event type pattern (if configured)
                    self.route_by_event_type(&event, config.project_id).await?
                } else {
                    // Default routing by livemode flag
                    self.route_general_webhook(&event, config.project_id).await?
                };

                routes.push(route);
            }
        }

        // 5. Log events for all routes
        for route in &routes {
            self.log_webhook_event(route, &event, "processed").await?;
        }

        Ok(routes)
    }

    fn has_price_id(&self, event: &stripe::Event) -> bool {
        // Check if event contains price information
        matches!(
            event.type_,
            EventType::InvoicePaymentSucceeded
                | EventType::InvoicePaymentFailed
                | EventType::CustomerSubscriptionCreated
                | EventType::CustomerSubscriptionUpdated
                | EventType::CustomerSubscriptionDeleted
        )
    }

    fn extract_price_id(&self, event: &stripe::Event) -> Result<String, RoutingError> {
        // Extract price_id from event payload
        match event.type_ {
            EventType::InvoicePaymentSucceeded | EventType::InvoicePaymentFailed => {
                if let EventObject::Invoice(invoice) = &event.data.object {
                    if let Some(lines) = &invoice.lines {
                        if let Some(line) = lines.data.first() {
                            if let Some(price) = &line.price {
                                return Ok(price.id.to_string());
                            }
                        }
                    }
                }
            }
            EventType::CustomerSubscriptionCreated
            | EventType::CustomerSubscriptionUpdated => {
                if let EventObject::Subscription(subscription) = &event.data.object {
                    if let Some(item) = subscription.items.data.first() {
                        return Ok(item.price.id.to_string());
                    }
                }
            }
            _ => {}
        }

        Err(RoutingError::NoPriceIdInPayload)
    }

    async fn route_by_price_id(
        &self,
        event: &stripe::Event,
        price_id: String,
    ) -> Result<WebhookRoute, RoutingError> {
        // Find specific routing rule for this price_id
        let rule = webhook_routing_rules::Entity::find()
            .filter(webhook_routing_rules::Column::ProviderType.eq("stripe"))
            .filter(webhook_routing_rules::Column::FilterType.eq("price_id"))
            .filter(webhook_routing_rules::Column::FilterValue.eq(&price_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| RoutingError::NoRoutingRule(price_id.clone()))?;

        Ok(WebhookRoute {
            project_id: rule.project_id,
            environment_id: rule.environment_id,
            event: event.clone(),
            routing_method: "price_id",
        })
    }

    async fn has_event_type_rule(
        &self,
        event: &stripe::Event,
        project_id: i32,
    ) -> Result<bool, RoutingError> {
        let count = webhook_routing_rules::Entity::find()
            .filter(webhook_routing_rules::Column::ProjectId.eq(project_id))
            .filter(webhook_routing_rules::Column::FilterType.eq("event_type"))
            .count(self.db.as_ref())
            .await?;

        Ok(count > 0)
    }
}

// Update return type
pub struct WebhookRoute {
    pub project_id: i32,
    pub environment_id: i32,
    pub event: stripe::Event,
    pub routing_method: String,
}
```

### UI for General Webhook Configuration

```
┌─────────────────────────────────────────────────────────────────┐
│  Webhook Routing Strategy                                       │
├─────────────────────────────────────────────────────────────────┤
│  How should general webhooks (non-transactional) be routed?    │
│                                                                 │
│  ○ Default by Mode (Recommended)                               │
│    • Live mode webhooks → Production                           │
│    • Test mode webhooks → Staging                              │
│    ℹ️ Simplest option, works for most use cases               │
│                                                                 │
│  ○ Custom Event Type Rules                                     │
│    Configure specific event types to route to specific envs   │
│    ⚠️ Requires manual configuration for each event type       │
│                                                                 │
│  [Save]                                                         │
└─────────────────────────────────────────────────────────────────┘
```

### Event Log with Routing Method

```
┌─────────────────────────────────────────────────────────────────┐
│  Webhook Event Log                                              │
├─────────────────────────────────────────────────────────────────┤
│  ✅ customer.created                            2 min ago       │
│     Mode: Test → Staging                                        │
│     Routing: Default by mode (no price_id)                     │
│     [View Details]                                              │
├─────────────────────────────────────────────────────────────────┤
│  ✅ invoice.payment_succeeded                   5 min ago       │
│     Mode: Live → Production                                     │
│     Routing: By price_id (price_1MonthlyProd_abc123)           │
│     [View Details]                                              │
├─────────────────────────────────────────────────────────────────┤
│  ✅ customer.updated                            10 min ago      │
│     Mode: Live → Production                                     │
│     Routing: Event type pattern (customer.*)                   │
│     [View Details]                                              │
└─────────────────────────────────────────────────────────────────┘
```

### Summary: General Webhook Routing

| Event Type | Has Price ID? | Routing Strategy | Configuration Needed |
|------------|---------------|------------------|---------------------|
| `invoice.payment_succeeded` | ✅ Yes | By price_id | Price ID mapping |
| `customer.subscription.created` | ✅ Yes | By price_id | Price ID mapping |
| `customer.created` | ❌ No | Default by mode | None |
| `customer.updated` | ❌ No | Default by mode | None (or event type rule) |
| `payment_method.attached` | ❌ No | Default by mode | None (or event type rule) |
| `charge.dispute.created` | ❌ No | Default by mode | None (or event type rule) |

**MVP Behavior:**
1. Events with price_id → Route by price_id (explicit rules required)
2. Events without price_id → Route by livemode flag (no configuration needed)
3. Live mode → Production environment
4. Test mode → Staging environment (highest priority)

**Future Enhancement:**
- Users can optionally configure event type patterns for fine-grained control
- Example: Route all `customer.*` events to Production, all `payment_method.*` to Staging

## Database Schema

### stripe_webhook_configs
Stores webhook secrets for Stripe accounts (shared across projects).

```sql
CREATE TABLE stripe_webhook_configs (
  id SERIAL PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  stripe_account_id VARCHAR(255) NOT NULL,

  -- Webhook secrets (encrypted with EncryptionService)
  webhook_secret_live TEXT NOT NULL,
  webhook_secret_test TEXT NOT NULL,

  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

  -- One Stripe account per project
  UNIQUE(project_id),

  -- Index for account lookup
  INDEX idx_stripe_account (stripe_account_id)
);
```

**Note**: Multiple projects can share the same `stripe_account_id` (e.g., company with multiple products).

### webhook_routing_rules
Defines which price IDs route to which environments (managed via UI).

```sql
CREATE TABLE webhook_routing_rules (
  id SERIAL PRIMARY KEY,

  -- Target
  environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,

  -- Filter
  provider_type VARCHAR(50) NOT NULL DEFAULT 'stripe',
  filter_type VARCHAR(50) NOT NULL DEFAULT 'price_id',
  filter_value VARCHAR(255) NOT NULL,

  -- Metadata
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  created_by INTEGER REFERENCES users(id),

  -- One price can only route to one environment
  UNIQUE(provider_type, filter_type, filter_value),

  -- Indexes
  INDEX idx_webhook_routing_lookup (provider_type, filter_type, filter_value),
  INDEX idx_webhook_routing_env (environment_id)
);
```

**Design Notes**:
- `filter_type` supports future expansion: `customer_id`, `subscription_id`, `metadata_key`
- `UNIQUE` constraint prevents duplicate price ID mappings
- `ON DELETE CASCADE` automatically removes rules when environment is deleted

### webhook_events
Logs all webhook events for debugging and auditing.

```sql
CREATE TABLE webhook_events (
  id SERIAL PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  environment_id INTEGER REFERENCES environments(id) ON DELETE SET NULL,

  -- Webhook details
  provider_type VARCHAR(50) NOT NULL,
  provider_mode VARCHAR(20) NOT NULL,  -- 'live' or 'test'
  provider_account_id VARCHAR(255) NOT NULL,
  event_id VARCHAR(255) NOT NULL,
  event_type VARCHAR(100) NOT NULL,

  -- Payload and routing
  payload JSONB NOT NULL,
  routing_method VARCHAR(50) NOT NULL,  -- 'price_id', 'fallback', 'manual'

  -- Status
  status VARCHAR(50) NOT NULL,  -- 'processed', 'failed', 'pending'
  error_message TEXT,

  -- Timestamps
  received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  processed_at TIMESTAMPTZ,

  -- Prevent duplicate processing
  UNIQUE(provider_type, event_id),

  -- Indexes
  INDEX idx_webhook_events_project (project_id),
  INDEX idx_webhook_events_environment (environment_id),
  INDEX idx_webhook_events_received (received_at DESC),
  INDEX idx_webhook_events_status (status)
);
```

**Design Notes**:
- `event_id` uniqueness ensures idempotency (same webhook not processed twice)
- `JSONB` payload allows querying webhook data
- `environment_id` can be NULL for unrouted webhooks
- Keep events for 90 days for debugging

## API Endpoints

### Webhook Ingestion

#### POST /api/_temps/stripe/webhook
Receive Stripe webhook events.

**Request:**
- Headers: `stripe-signature` (required for verification)
- Body: Raw JSON webhook payload

**Response:**
- `200 OK`: Webhook processed successfully
- `400 Bad Request`: Invalid signature or payload
- `404 Not Found`: Unknown Stripe account (not configured)
- `500 Internal Server Error`: Processing error

**Example:**
```bash
curl -X POST https://temps.io/api/_temps/stripe/webhook \
  -H "stripe-signature: t=1234,v1=abc..." \
  -d '{"id":"evt_123","type":"invoice.payment_succeeded",...}'
```

### Configuration Management

#### POST /api/projects/{project_id}/integrations/stripe/config
Configure Stripe webhook secrets for a project.

**Request:**
```json
{
  "stripe_account_id": "acct_1A2B3C4D5E6F7G",
  "webhook_secret_live": "whsec_live_...",
  "webhook_secret_test": "whsec_test_..."
}
```

**Response:**
```json
{
  "id": 1,
  "project_id": 12,
  "stripe_account_id": "acct_1A2B3C4D5E6F7G",
  "created_at": "2025-02-05T14:30:00Z"
}
```

#### POST /api/projects/{project_id}/integrations/stripe/routing-rules
Add a price ID routing rule.

**Request:**
```json
{
  "environment_id": 5,
  "price_id": "price_1MonthlyProd_abc123"
}
```

**Response:**
```json
{
  "id": 42,
  "environment_id": 5,
  "environment_name": "Production",
  "price_id": "price_1MonthlyProd_abc123",
  "created_at": "2025-02-05T14:30:00Z"
}
```

**Errors:**
- `409 Conflict`: Price ID already configured for another environment

#### DELETE /api/projects/{project_id}/integrations/stripe/routing-rules/{rule_id}
Remove a routing rule.

**Response:** `204 No Content`

#### GET /api/projects/{project_id}/integrations/stripe/routing-rules
List all routing rules for a project.

**Response:**
```json
[
  {
    "id": 42,
    "environment_id": 5,
    "environment_name": "Production",
    "price_id": "price_1MonthlyProd_abc123",
    "created_at": "2025-02-05T14:30:00Z"
  },
  {
    "id": 43,
    "environment_id": 5,
    "environment_name": "Production",
    "price_id": "price_1YearlyProd_def456",
    "created_at": "2025-02-05T14:31:00Z"
  }
]
```

#### GET /api/projects/{project_id}/webhooks/events
List webhook events for a project.

**Query Parameters:**
- `environment_id` (optional): Filter by environment
- `status` (optional): Filter by status (`processed`, `failed`, `pending`)
- `limit` (optional): Number of events to return (default: 50, max: 100)
- `offset` (optional): Pagination offset

**Response:**
```json
{
  "data": [
    {
      "id": 123,
      "event_id": "evt_1234567890",
      "event_type": "invoice.payment_succeeded",
      "provider_mode": "live",
      "environment_name": "Production",
      "status": "processed",
      "received_at": "2025-02-05T14:30:00Z"
    }
  ],
  "total": 1234,
  "limit": 50,
  "offset": 0
}
```

## User Flows

### Initial Setup (First-Time User)

**Time: ~5 minutes**

1. User creates project in Temps
2. User navigates to **Project Settings → Integrations → Stripe**
3. User clicks **"Connect Stripe Webhooks"**
4. Temps shows webhook URL: `https://temps.io/api/_temps/stripe/webhook`
5. User opens Stripe Dashboard → Webhooks
6. User creates endpoint in **Live mode**:
   - URL: `https://temps.io/api/_temps/stripe/webhook`
   - Events: "Send all event types" (or specific events)
   - Copies signing secret: `whsec_live_...`
7. User repeats for **Test mode** (same URL, different secret)
8. User returns to Temps and enters:
   - Stripe account ID: `acct_1A2B3C4D5E6F7G`
   - Live webhook secret: `whsec_live_...`
   - Test webhook secret: `whsec_test_...`
9. Temps validates configuration (sends test webhook)
10. ✅ Setup complete!

### Adding Price ID Routing

**Time: ~30 seconds per price**

1. User navigates to **Project Settings → Integrations → Stripe**
2. User selects environment (e.g., "Production")
3. User clicks **"Add Price"**
4. User enters price ID: `price_1MonthlyProd_abc123`
5. User clicks **"Add"**
6. ✅ Routing rule created immediately
7. Webhooks for this price now route to Production

### Handling Unrouted Webhooks

**Scenario**: Webhook arrives for a price ID that has no routing rule.

1. Temps receives webhook for `price_1Unknown_xyz`
2. No routing rule found
3. Temps logs event with `status: 'pending'`
4. User sees notification: **"1 unrouted webhook"**
5. User clicks notification → Views webhook details
6. User sees price ID and event details
7. User selects environment from dropdown
8. User clicks **"Assign to [Environment]"**
9. Optional: User checks **"Remember this price for future"** (creates routing rule)
10. Webhook is reprocessed and routed to selected environment

## Implementation Plan

### Phase 1: Core Infrastructure (MVP)
**Goal**: Single webhook endpoint with UI-based routing

**Tasks:**
1. Create `temps-webhooks` crate
2. Implement database schema (migrations)
3. Create webhook handler for Stripe
4. Implement routing service with price_id matching
5. Add webhook signature verification
6. Build UI for Stripe configuration
7. Build UI for price ID management
8. Create webhook event log viewer
9. Write integration tests

**Success Criteria:**
- Users can configure Stripe webhooks via UI
- Webhooks automatically route to correct environment
- Event log shows all webhook activity
- Signature verification prevents spoofing

### Phase 2: Enhanced Features
**Goal**: Better visibility and error handling

**Tasks:**
1. Implement unrouted webhook queue
2. Add manual webhook assignment UI
3. Add webhook retry mechanism
4. Implement audit logging for configuration changes
5. Add webhook testing from UI (send test events)
6. Create dashboard with webhook statistics

### Phase 3: Additional Providers
**Goal**: Support multiple payment providers

**Tasks:**
1. Add Lemon Squeezy webhook handler
2. Add Paddle webhook handler
3. Generalize routing service for any provider
4. Add provider-specific configuration UIs

### Phase 4: Advanced Routing
**Goal**: More flexible routing options

**Tasks:**
1. Add routing by `customer_id`
2. Add routing by `subscription_id`
3. Add routing by Stripe metadata
4. Add conditional routing (event type filters)
5. Add priority-based routing for conflicts

## Security Considerations

### Webhook Signature Verification
**Critical**: Every webhook must be verified before processing.

```rust
// Verify Stripe signature
stripe::Webhook::construct_event(
    raw_body,
    signature_header,
    webhook_secret,
)
.map_err(|_| RoutingError::InvalidSignature)?;
```

**Protection Against:**
- Spoofed webhooks (attacker cannot forge signature)
- Replay attacks (signature includes timestamp)
- Man-in-the-middle (signature proves authenticity)

### Secret Storage
**Requirement**: Webhook secrets must be encrypted at rest.

```rust
// Encrypt before storing
let encrypted_secret = encryption_service.encrypt(&webhook_secret)?;

stripe_webhook_configs::ActiveModel {
    webhook_secret_live: Set(encrypted_secret),
    ...
}
```

**Use**: `temps_core::EncryptionService` with 32-byte AES-256-GCM key.

### Rate Limiting
**Requirement**: Protect webhook endpoint from abuse.

**Strategy**:
- Per-IP rate limit: 100 requests/minute
- Per-account rate limit: 1000 requests/minute
- Return `429 Too Many Requests` when exceeded

### Permissions
**Requirement**: Control who can configure webhooks.

**Permissions**:
- `WebhooksRead`: View webhook configuration and events
- `WebhooksWrite`: Add/remove routing rules
- `WebhooksAdmin`: Configure webhook secrets

## Performance Considerations

### Database Queries
**Challenge**: Webhook routing requires database lookup on every request.

**Optimization**:
1. **Indexing**:
   - `idx_stripe_account` on `stripe_webhook_configs.stripe_account_id`
   - `idx_webhook_routing_lookup` on `(provider_type, filter_type, filter_value)`
2. **Caching**: Cache routing rules in Redis (TTL: 5 minutes)
3. **Connection Pooling**: Use existing database connection pool

**Expected Latency**:
- Without cache: ~10-20ms (database query)
- With cache: ~1-2ms (Redis lookup)
- Total webhook processing: ~50-100ms

### Scaling
**Challenge**: Support thousands of projects with high webhook volume.

**Strategies**:
1. **Horizontal Scaling**: Multiple webhook handler instances behind load balancer
2. **Async Processing**: Use message queue for webhook processing (optional)
3. **Database Read Replicas**: Route read queries to replicas
4. **Event Log Retention**: Archive old events (>90 days) to cold storage

**Capacity Estimate**:
- 1000 projects × 100 webhooks/day = 100,000 webhooks/day
- Peak: ~1-2 webhooks/second
- Single instance can handle: ~100-500 webhooks/second

## Monitoring and Observability

### Metrics to Track
1. **Webhook Volume**: Requests per second (by provider, by project)
2. **Routing Success Rate**: % of webhooks successfully routed
3. **Signature Verification Failures**: Invalid signatures (potential attacks)
4. **Unrouted Webhooks**: Webhooks with no matching rule
5. **Processing Latency**: P50, P95, P99 latency
6. **Error Rate**: Failed webhook processing

### Logging
**Structure**: Use structured logging with log levels.

```rust
info!(
    "Webhook routed successfully",
    provider = "stripe",
    event_type = "invoice.payment_succeeded",
    price_id = price_id,
    project_id = project_id,
    environment_id = environment_id,
    latency_ms = elapsed.as_millis(),
);
```

### Alerts
**Conditions**:
1. Signature verification failure rate > 1%
2. Unrouted webhook rate > 10%
3. Webhook processing latency P95 > 1 second
4. Error rate > 5%

## Testing Strategy

### Unit Tests
**Scope**: Test routing logic in isolation.

```rust
#[tokio::test]
async fn test_route_by_price_id() {
    let service = setup_routing_service().await;

    let webhook = create_test_webhook("price_1MonthlyProd_abc123");
    let route = service.route_stripe_webhook(webhook).await.unwrap();

    assert_eq!(route.environment_id, 5); // Production
}

#[tokio::test]
async fn test_unknown_price_returns_error() {
    let service = setup_routing_service().await;

    let webhook = create_test_webhook("price_unknown");
    let result = service.route_stripe_webhook(webhook).await;

    assert!(matches!(result, Err(RoutingError::NoRoutingRule(_))));
}
```

### Integration Tests
**Scope**: Test full webhook flow end-to-end.

```rust
#[tokio::test]
async fn test_webhook_routing_end_to_end() {
    let app = setup_test_app().await;

    // Configure webhook
    app.create_stripe_config(project_id, account_id, secrets).await;
    app.create_routing_rule(environment_id, "price_test_123").await;

    // Send webhook
    let response = app
        .post("/api/_temps/stripe/webhook")
        .header("stripe-signature", valid_signature)
        .json(&webhook_payload)
        .send()
        .await;

    assert_eq!(response.status(), 200);

    // Verify routing
    let event = app.get_webhook_event(event_id).await;
    assert_eq!(event.environment_id, environment_id);
    assert_eq!(event.status, "processed");
}
```

### UI Tests
**Scope**: Test configuration UI with Playwright.

```typescript
test('configure stripe webhook routing', async ({ page }) => {
  await page.goto('/projects/1/settings/integrations')

  // Enter Stripe config
  await page.fill('[name="account_id"]', 'acct_123')
  await page.fill('[name="webhook_secret_live"]', 'whsec_live_...')
  await page.fill('[name="webhook_secret_test"]', 'whsec_test_...')
  await page.click('button:has-text("Save")')

  // Add routing rule
  await page.fill('[name="price_id"]', 'price_1Test_abc')
  await page.click('button:has-text("Add Price")')

  // Verify rule appears
  await expect(page.locator('text=price_1Test_abc')).toBeVisible()
})
```

## Migration Strategy

### Phase 1: Deploy Infrastructure (No User Impact)
1. Deploy `temps-webhooks` crate with disabled routes
2. Run database migrations
3. Deploy UI (hidden behind feature flag)
4. Smoke test in staging

### Phase 2: Enable for Beta Users
1. Enable feature flag for selected projects
2. Help users migrate from manual webhooks to Temps routing
3. Collect feedback and iterate

### Phase 3: General Availability
1. Announce feature to all users
2. Provide migration guide
3. Create video tutorial
4. Offer support during migration

## Future Enhancements

### 1. Webhook Replay
Allow users to replay past webhooks for debugging.

**UI**: Webhook event log → Click event → "Replay" button

### 2. Webhook Forwarding
Forward webhooks to external URLs (for custom integrations).

**Use Case**: User wants to receive webhooks in Slack, Discord, or custom service.

### 3. Webhook Transformation
Transform webhook payloads before forwarding.

**Use Case**: Normalize Stripe and Lemon Squeezy webhooks to common format.

### 4. Multi-Account Support
Support multiple Stripe accounts per project.

**Use Case**: Company with multiple brands, each with own Stripe account.

### 5. Webhook Analytics
Dashboard showing webhook volume, success rate, latency over time.

**Metrics**: Charts for webhook activity, popular event types, error trends.

## Open Questions

1. **Webhook Forwarding Method**: Should we forward webhooks via HTTP proxy, message queue, or database polling?
   - **Recommendation**: Start with HTTP proxy (simplest), add queue later for reliability.

2. **Event Retention**: How long should we keep webhook events?
   - **Recommendation**: 90 days in hot storage, archive to S3 for 1 year.

3. **Price ID Conflicts**: What happens if two environments want the same price ID?
   - **Recommendation**: UNIQUE constraint prevents this. Last deployment wins (with warning).

4. **Fallback Routing**: Should we have a "default" environment for unrouted webhooks?
   - **Recommendation**: No automatic fallback. Require explicit user assignment.

5. **Multi-Region**: How do we handle webhooks in multi-region deployments?
   - **Recommendation**: Single global endpoint, route to regional database via geo-routing.

## References

- [Stripe Webhooks Documentation](https://stripe.com/docs/webhooks)
- [Lemon Squeezy Webhooks](https://docs.lemonsqueezy.com/api/webhooks)
- [Webhook Security Best Practices](https://webhooks.fyi/best-practices/webhook-security)
- [RFC 7807 - Problem Details for HTTP APIs](https://datatracker.ietf.org/doc/html/rfc7807)

## Changelog

- **2025-02-05**: Initial architecture proposal
- **2025-02-05**: Added comprehensive section on handling general webhooks (customer.created, customer.updated, etc.) that don't contain price_id information. Includes multiple routing strategies and hybrid approach recommendation.
