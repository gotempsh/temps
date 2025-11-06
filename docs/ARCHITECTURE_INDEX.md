# Temps Architecture Documentation Index

A comprehensive guide to all architecture documentation for Temps.

## Quick Navigation

### For Different Audiences

#### 👤 New Developers
Start here to understand the project:
1. **[README.md](../README.md)** - Project overview and quick start
2. **[ARCHITECTURE.md](../ARCHITECTURE.md)** - System overview and high-level architecture
3. **[PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)** - How to extend Temps with plugins

#### 🔧 Reverse Proxy Engineers
Deep dive into the load balancer:
1. **[PINGORA_LOAD_BALANCER.md](./PINGORA_LOAD_BALANCER.md)** - Pingora configuration and optimization
2. **[ARCHITECTURE.md#pingora-load-balancer](../ARCHITECTURE.md#pingora-load-balancer)** - Pingora integration
3. **[crates/temps-proxy/README.md](../crates/temps-proxy/README.md)** - Proxy implementation details

#### 🛠️ Plugin Developers
Building plugins for Temps:
1. **[PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)** - Complete plugin development guide
2. **[ARCHITECTURE.md#plugin-system](../ARCHITECTURE.md#plugin-system)** - Plugin architecture overview
3. **Example Plugins** - See existing plugins: `crates/temps-proxy/`, `crates/temps-deployer/`

#### 📊 DevOps/Deployment Engineers
Deploying and managing Temps:
1. **[README.md#installation](../README.md#-installation)** - Installation options
2. **[ARCHITECTURE.md#deployment-pipeline](../ARCHITECTURE.md#deployment-pipeline)** - How deployments work
3. **[ARCHITECTURE.md#configuration](../ARCHITECTURE.md#configuration)** - Configuration guide

#### 🔒 Security Engineers
Understanding security architecture:
1. **[SECURITY_IMPLEMENTATION_GUIDE.md](../SECURITY_IMPLEMENTATION_GUIDE.md)** - Security features
2. **[ARCHITECTURE.md#security-architecture](../ARCHITECTURE.md#security-architecture)** - Security layers
3. **[PINGORA_LOAD_BALANCER.md#security-hardening](./PINGORA_LOAD_BALANCER.md#security-hardening)** - Proxy security

#### 📈 Observability Engineers
Monitoring and analytics:
1. **[ARCHITECTURE.md#monitoring--logging](./PINGORA_LOAD_BALANCER.md#monitoring--logging)** - Pingora metrics
2. **[ARCHITECTURE.md#data-flow](../ARCHITECTURE.md#data-flow)** - Analytics data flow
3. **[BACKEND_API_ANALYSIS.md](../BACKEND_API_ANALYSIS.md)** - API endpoints for observability

---

## Documentation Files

### Core Architecture Documents

```
ARCHITECTURE.md (This is the main architecture document)
├── System Overview
├── Pingora Load Balancer
├── Plugin System
├── Request Flow
├── Deployment Pipeline
├── Data Flow
├── Database Layer
├── Crate Organization
├── Security Architecture
└── Configuration

docs/PLUGIN_SYSTEM.md (Plugin development guide)
├── Overview
├── Creating a Plugin
├── Service Registration
├── Route Configuration
├── OpenAPI Integration
├── Plugin Lifecycle
├── Dynamic Loading (.so support)
├── Examples
└── Best Practices

docs/PINGORA_LOAD_BALANCER.md (Load balancer configuration)
├── Overview
├── Pingora Integration
├── Request Processing (6 phases)
├── TLS/SSL Configuration
├── Load Balancing
├── Performance Tuning
├── Monitoring & Logging
├── Security Hardening
├── Troubleshooting
└── Advanced Configuration
```

### Supporting Documents

| Document | Purpose | Audience |
|----------|---------|----------|
| **README.md** | Project overview, quick start, features | Everyone |
| **SECURITY_IMPLEMENTATION_GUIDE.md** | Security features and implementation | Security engineers |
| **BACKEND_API_ANALYSIS.md** | Complete API reference and crate analysis | API developers |
| **TEMPS_FUNCTIONALITY_OVERVIEW.md** | User-facing features documentation | Product managers, users |
| **CHANGELOG.md** | Version history and breaking changes | Release managers |

### Crate-Specific Documentation

| Crate | README | Purpose |
|-------|--------|---------|
| **temps-proxy** | `crates/temps-proxy/README.md` | Reverse proxy with Pingora |
| **temps-deployer** | `crates/temps-deployer/README.md` | Container building and deployment |
| **temps-domains** | `crates/temps-domains/README.md` | DNS and TLS certificate management |
| **temps-database** | `crates/temps-database/README.md` | PostgreSQL database layer |
| **temps-analytics** | `crates/temps-analytics/README.md` | Analytics engine and metrics |
| **temps-auth** | `crates/temps-auth/README.md` | Authentication and authorization |

---

## Architecture Components

### System Layers

```
┌─────────────────────────────────────┐
│     Client Layer                    │
│  (Web UI, Git Providers)            │
└─────────────────┬───────────────────┘
                  │
┌─────────────────▼───────────────────┐
│     Pingora Proxy Layer             │
│  (Load Balancing, TLS)              │
│  Docs: PINGORA_LOAD_BALANCER.md     │
└─────────────────┬───────────────────┘
                  │
┌─────────────────▼───────────────────┐
│     Temps Application Layer         │
│  (Axum, Plugins, Services)          │
│  Docs: ARCHITECTURE.md              │
└─────────────────┬───────────────────┘
                  │
┌─────────────────▼───────────────────┐
│     Data Layer                      │
│  (PostgreSQL, Redis, S3)            │
│  Docs: ARCHITECTURE.md#database     │
└─────────────────────────────────────┘
```

### Plugin Architecture

See **[PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)** for complete plugin development guide.

Key plugins:
- **ProxyPlugin** - HTTP/HTTPS request routing and analytics
- **DeployerPlugin** - Container building and CI/CD
- **DomainsPlugin** - DNS and TLS certificate management
- **AnalyticsPlugin** - Event tracking and metrics
- **AuthPlugin** - User authentication and authorization
- **40+ other plugins** - Various features and integrations

### Request Flow

```
Client Request
    ↓
Pingora TLS Termination (PINGORA_LOAD_BALANCER.md)
    ↓
ProxyHttp.select_upstream() (PINGORA_LOAD_BALANCER.md#request-processing)
    ↓
Project Context Resolution (ARCHITECTURE.md#request-flow)
    ↓
IP Access Control (ARCHITECTURE.md#security-architecture)
    ↓
CAPTCHA Challenge (optional)
    ↓
Forward to Deployment (ARCHITECTURE.md#deployment-pipeline)
    ↓
Response Modification
    ↓
Analytics Logging (ARCHITECTURE.md#data-flow)
    ↓
Send to Client
```

---

## Key Design Patterns

### Plugin Architecture

All functionality is organized as plugins implementing the `TempsPlugin` trait.

**Document**: [PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)

```
TempsPlugin trait
├── name() - Plugin identifier
├── register_services() - Setup and initialization
├── configure_routes() - HTTP route handlers
└── openapi_schema() - API documentation
```

### Service Registration

Type-safe dependency injection through the ServiceRegistry.

**Document**: [PLUGIN_SYSTEM.md#service-registration](./PLUGIN_SYSTEM.md#service-registration)

```
ServiceRegistrationContext
├── register_service(Arc<Service>)
└── get_service::<Service>() → Option<Arc<Service>>
```

### Request Processing Phases

Pingora's ProxyHttp trait provides 6 phases for request handling.

**Document**: [PINGORA_LOAD_BALANCER.md#request-processing](./PINGORA_LOAD_BALANCER.md#request-processing)

```
1. Early Phase (select_upstream)
2. Modify Phase (request_filter)
3. Proxy Phase (upstream_peer)
4. Response Phase (upstream_response_filter)
5. Filter Phase (response_filter)
6. Finish Phase (logging)
```

### Three-Layer Architecture

HTTP Handlers → Service Layer → Database Layer

**Document**: [ARCHITECTURE.md#three-layer-architecture-pattern](../ARCHITECTURE.md#three-layer-architecture-pattern)

---

## Common Development Tasks

### Adding a New Endpoint

1. Read: [PLUGIN_SYSTEM.md#route-configuration](./PLUGIN_SYSTEM.md#route-configuration)
2. Create a route handler
3. Register in plugin's `configure_routes()`
4. Document with OpenAPI/utoipa

### Creating a Plugin

1. Follow: [PLUGIN_SYSTEM.md#creating-a-plugin](./PLUGIN_SYSTEM.md#creating-a-plugin)
2. Implement `TempsPlugin` trait
3. Register services
4. Configure routes
5. Add to bootstrap

### Understanding Request Flow

1. Start: [ARCHITECTURE.md#request-flow](../ARCHITECTURE.md#request-flow)
2. Deep dive: [PINGORA_LOAD_BALANCER.md#request-processing](./PINGORA_LOAD_BALANCER.md#request-processing)
3. Trace analytics: [ARCHITECTURE.md#data-flow](../ARCHITECTURE.md#data-flow)

### Deploying an Application

1. Read: [ARCHITECTURE.md#deployment-pipeline](../ARCHITECTURE.md#deployment-pipeline)
2. Check: [crates/temps-deployer/README.md](../crates/temps-deployer/README.md)
3. Reference: [README.md#deploying-your-first-application](../README.md#-deploying-your-first-application)

### Configuring TLS Certificates

1. Read: [PINGORA_LOAD_BALANCER.md#tlsssl-configuration](./PINGORA_LOAD_BALANCER.md#tlsssl-configuration)
2. Check: [crates/temps-domains/README.md](../crates/temps-domains/README.md)
3. Debug: [PINGORA_LOAD_BALANCER.md#troubleshooting](./PINGORA_LOAD_BALANCER.md#troubleshooting)

### Monitoring and Debugging

1. Metrics: [PINGORA_LOAD_BALANCER.md#prometheus-metrics](./PINGORA_LOAD_BALANCER.md#prometheus-metrics)
2. Logging: [PINGORA_LOAD_BALANCER.md#request-logging](./PINGORA_LOAD_BALANCER.md#request-logging)
3. Analytics: [ARCHITECTURE.md#data-flow](../ARCHITECTURE.md#data-flow)
4. Troubleshooting: [PINGORA_LOAD_BALANCER.md#troubleshooting](./PINGORA_LOAD_BALANCER.md#troubleshooting)

---

## File Structure

```
temps/
├── ARCHITECTURE.md                  ← Main architecture doc (mermaid diagrams)
├── SECURITY_IMPLEMENTATION_GUIDE.md ← Security features
├── BACKEND_API_ANALYSIS.md          ← API endpoints and crates
├── README.md                        ← Quick start and features
├── TEMPS_FUNCTIONALITY_OVERVIEW.md  ← User features
│
├── docs/
│   ├── ARCHITECTURE_INDEX.md        ← This file
│   ├── PLUGIN_SYSTEM.md             ← Plugin development guide (mermaid diagrams)
│   ├── PINGORA_LOAD_BALANCER.md     ← Pingora configuration (mermaid diagrams)
│   ├── RELEASING.md                 ← Release procedures
│   └── architecture/
│       └── payment-webhook-routing.md
│
├── crates/
│   ├── temps-proxy/
│   │   ├── README.md                ← Proxy implementation details
│   │   └── src/
│   │       ├── proxy.rs             ← LoadBalancer + ProxyHttp trait
│   │       ├── server.rs            ← Server setup
│   │       ├── plugin.rs            ← Plugin registration
│   │       └── handler/             ← HTTP handlers
│   │
│   ├── temps-deployer/
│   │   ├── README.md                ← Deployment details
│   │   └── src/plugin.rs            ← Deployer plugin
│   │
│   ├── temps-domains/
│   │   ├── README.md                ← Domain/TLS details
│   │   └── src/plugin.rs            ← Domains plugin
│   │
│   ├── temps-database/
│   │   ├── README.md                ← Database layer
│   │   └── src/lib.rs
│   │
│   ├── temps-core/
│   │   └── src/plugin.rs            ← Plugin trait definitions
│   │
│   └── [35+ other crates]
│
└── web/
    └── src/                         ← React frontend
```

---

## Mermaid Diagrams Used

This architecture documentation uses Mermaid diagrams extensively:

- **graph TB/LR** - Flowcharts for processes
- **sequenceDiagram** - Request/response flows
- **erDiagram** - Database relationships
- **graph TB** - System architecture

All diagrams are rendered in:
- [ARCHITECTURE.md](../ARCHITECTURE.md)
- [PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)
- [PINGORA_LOAD_BALANCER.md](./PINGORA_LOAD_BALANCER.md)

---

## Learning Path

### Week 1: Foundation
- [ ] Read [README.md](../README.md)
- [ ] Run quick start
- [ ] Read [ARCHITECTURE.md#system-overview](../ARCHITECTURE.md#system-overview)
- [ ] Explore `crates/` directory structure

### Week 2: Proxy & Routing
- [ ] Read [PINGORA_LOAD_BALANCER.md#overview](./PINGORA_LOAD_BALANCER.md#overview)
- [ ] Study [PINGORA_LOAD_BALANCER.md#request-processing](./PINGORA_LOAD_BALANCER.md#request-processing)
- [ ] Explore `crates/temps-proxy/src/`
- [ ] Understand TLS certificate loading

### Week 3: Plugins
- [ ] Read [PLUGIN_SYSTEM.md](./PLUGIN_SYSTEM.md)
- [ ] Study existing plugins
- [ ] Create a simple test plugin
- [ ] Write tests for plugin

### Week 4: Advanced
- [ ] Deep dive into specific components
- [ ] Understand database schema
- [ ] Learn deployment pipeline
- [ ] Review analytics flow

---

## Reference Sections

### Frequently Needed Info

**"How do I add an API endpoint?"**
→ [PLUGIN_SYSTEM.md#route-configuration](./PLUGIN_SYSTEM.md#route-configuration)

**"How does a request flow through the system?"**
→ [PINGORA_LOAD_BALANCER.md#request-processing](./PINGORA_LOAD_BALANCER.md#request-processing)

**"How are certificates loaded?"**
→ [PINGORA_LOAD_BALANCER.md#tlsssl-configuration](./PINGORA_LOAD_BALANCER.md#tlsssl-configuration)

**"How do deployments work?"**
→ [ARCHITECTURE.md#deployment-pipeline](../ARCHITECTURE.md#deployment-pipeline)

**"What services are available?"**
→ [ARCHITECTURE.md#crate-organization](../ARCHITECTURE.md#crate-organization)

**"How is data stored?"**
→ [ARCHITECTURE.md#database-layer](../ARCHITECTURE.md#database-layer)

**"What are security features?"**
→ [SECURITY_IMPLEMENTATION_GUIDE.md](../SECURITY_IMPLEMENTATION_GUIDE.md)

**"How do I debug issues?"**
→ [PINGORA_LOAD_BALANCER.md#troubleshooting](./PINGORA_LOAD_BALANCER.md#troubleshooting)

---

## Contributing to Documentation

When adding new features:

1. Update relevant `.md` file
2. Add mermaid diagrams if showing flows
3. Include code examples
4. Link from this index
5. Update the relevant crate's README

---

## External Resources

- **Pingora**: https://github.com/cloudflare/pingora
- **Axum**: https://github.com/tokio-rs/axum
- **Sea-ORM**: https://www.sea-ql.org/
- **Tokio**: https://tokio.rs/
- **Utoipa**: https://github.com/juhaku/utoipa

---

## Summary

This documentation provides:

✅ **System-wide architecture** with mermaid diagrams
✅ **Plugin development guide** for extending Temps
✅ **Pingora load balancer** configuration and tuning
✅ **Request flow** from client to response
✅ **Deployment pipeline** documentation
✅ **Security architecture** overview
✅ **Data flow** for analytics
✅ **Database schema** and relationships
✅ **Troubleshooting** guides
✅ **Best practices** for development

Start with your role/interest from the top of this document, then follow the recommended reading path!
