---
name: deploy-to-temps
description: |
  Deploy applications to the Temps platform with automatic framework detection, Dockerfile generation, and container orchestration. Supports Next.js, Vite, React, Node.js, Python, Go, Rust, Java, and C# applications. Use when the user wants to: (1) Deploy their app to Temps, (2) Set up CI/CD with Temps, (3) Configure deployment settings, (4) Create a Dockerfile for Temps, (5) Deploy a containerized application, (6) Set up automatic deployments from Git. Triggers: "deploy to temps", "temps deployment", "push to temps", "containerize for temps", "temps ci/cd".
---

# Deploy to Temps

Deploy applications to Temps with automatic framework detection and optimized builds.

## Supported Frameworks

| Framework | Detection | Build Command |
|-----------|-----------|---------------|
| Next.js | `next.config.*` | `next build` |
| Vite | `vite.config.*` | `vite build` |
| Create React App | `react-scripts` in package.json | `react-scripts build` |
| Remix | `remix.config.*` | `remix build` |
| Express/Node.js | `express` in dependencies | `npm run build` (if exists) |
| NestJS | `@nestjs/core` in dependencies | `nest build` |
| Python/Flask | `requirements.txt` + `app.py` | - |
| Python/Django | `manage.py` | `python manage.py collectstatic` |
| Go | `go.mod` | `go build` |
| Rust | `Cargo.toml` | `cargo build --release` |

## Quick Deploy

### Via Git Integration

1. Connect your Git provider in Temps dashboard
2. Select repository and branch
3. Temps auto-detects framework and deploys

### Via CLI

```bash
# Install Temps CLI
npm install -g @temps-sdk/cli

# Login
temps login

# Deploy current directory
temps deploy

# Deploy with specific settings
temps deploy --project my-app --branch main
```

## Dockerfile Generation

Temps auto-generates optimized Dockerfiles. For custom needs:

### Next.js (Standalone)

```dockerfile
FROM node:20-alpine AS base

FROM base AS deps
WORKDIR /app
COPY package*.json ./
RUN npm ci

FROM base AS builder
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

FROM base AS runner
WORKDIR /app
ENV NODE_ENV=production
RUN addgroup --system --gid 1001 nodejs
RUN adduser --system --uid 1001 nextjs

COPY --from=builder /app/public ./public
COPY --from=builder --chown=nextjs:nodejs /app/.next/standalone ./
COPY --from=builder --chown=nextjs:nodejs /app/.next/static ./.next/static

USER nextjs
EXPOSE 3000
ENV PORT=3000
CMD ["node", "server.js"]
```

### Node.js (Express/Fastify)

```dockerfile
FROM node:20-alpine
WORKDIR /app

COPY package*.json ./
RUN npm ci --only=production

COPY . .

ENV NODE_ENV=production
USER node
EXPOSE 3000
CMD ["node", "dist/index.js"]
```

### Python (Flask/FastAPI)

```dockerfile
FROM python:3.11-slim
WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY . .

ENV PYTHONUNBUFFERED=1
EXPOSE 8000
CMD ["gunicorn", "-w", "4", "-b", "0.0.0.0:8000", "app:app"]
```

### Go

```dockerfile
FROM golang:1.21-alpine AS builder
WORKDIR /app

COPY go.mod go.sum ./
RUN go mod download

COPY . .
RUN CGO_ENABLED=0 GOOS=linux go build -o main .

FROM alpine:latest
RUN apk --no-cache add ca-certificates
WORKDIR /root/

COPY --from=builder /app/main .
EXPOSE 8080
CMD ["./main"]
```

## Environment Variables

Configure in Temps dashboard or via CLI:

```bash
# Set environment variable
temps env set DATABASE_URL="postgres://..."

# Set from .env file
temps env import .env

# List variables
temps env list
```

## Build Configuration

Create `temps.json` in project root:

```json
{
  "name": "my-app",
  "framework": "nextjs",
  "buildCommand": "npm run build",
  "installCommand": "npm ci",
  "outputDirectory": ".next",
  "nodeVersion": "20",
  "env": {
    "NODE_ENV": "production"
  }
}
```

## Git-based Deployments

### Auto-deploy on Push

1. In Temps dashboard, enable "Auto-deploy"
2. Select branches to auto-deploy
3. Each push triggers a new deployment

### Preview Deployments

Enable "Preview deployments" to create unique URLs for each PR.

### Deploy Hooks

Create webhooks for custom CI/CD:

```bash
curl -X POST https://your-temps.com/api/projects/123/deploy \
  -H "Authorization: Bearer $TEMPS_DEPLOY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"branch": "main", "commit": "abc123"}'
```

## Rollbacks

```bash
# List deployments
temps deployments list

# Rollback to specific deployment
temps rollback --deployment-id 456

# Rollback to previous
temps rollback --previous
```

## Health Checks

Configure health checks in `temps.json`:

```json
{
  "healthCheck": {
    "path": "/api/health",
    "interval": 30,
    "timeout": 10,
    "unhealthyThreshold": 3
  }
}
```

## Resource Configuration

```json
{
  "resources": {
    "cpu": "0.5",
    "memory": "512Mi",
    "replicas": {
      "min": 1,
      "max": 5
    }
  }
}
```

## Troubleshooting

**Build fails?**
- Check build logs in dashboard
- Verify `buildCommand` is correct
- Ensure all dependencies are in package.json

**Container won't start?**
- Check `PORT` environment variable is used
- Verify health check endpoint works
- Review container logs

**Deployment stuck?**
- Check resource limits aren't exceeded
- Verify Docker image builds locally
- Review deployment logs
