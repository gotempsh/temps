---
name: deploy-to-temps
description: |
  Deploy applications to the Temps platform with automatic framework detection, Dockerfile generation, and container orchestration. Supports Next.js, Vite, React, Node.js, Python, Go, Rust, Java, and C# applications. Use when the user wants to: (1) Deploy their app to Temps, (2) Set up CI/CD with Temps, (3) Configure deployment settings, (4) Create a Dockerfile for Temps, (5) Deploy a containerized application, (6) Set up automatic deployments from Git. Triggers: "deploy to temps", "temps deployment", "push to temps", "containerize for temps", "temps ci/cd".
---

# Deploy to Temps

Deploy applications to Temps with automatic framework detection.

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
npm install -g @temps-sdk/cli
temps login
temps deploy
# Or with explicit settings:
temps deploy --project my-app --branch main
```

**Validate**: Run `temps whoami` after login to confirm authentication. After `temps deploy`, check the returned deployment URL responds with HTTP 200.

## Dockerfile Generation

Temps auto-generates Dockerfiles. For custom needs:

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

**Validate**: Build the Docker image locally with `docker build -t test .` and run it with `docker run -p 3000:3000 test` to confirm it starts and serves traffic before deploying.

## Environment Variables

```bash
temps env set DATABASE_URL="postgres://..."
temps env import .env   # Import from .env file
temps env list          # Verify all variables are set
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

**Validate**: Run `temps env list` to confirm variables are set. Missing variables cause runtime errors, not build errors.

## Git-based Deployments

### Auto-deploy on Push

1. Enable "Auto-deploy" in Temps dashboard and select branches
2. Each push triggers a new deployment

### Preview Deployments

Enable "Preview deployments" to generate unique URLs per PR.

### Deploy Hooks

```bash
curl -X POST https://your-temps.com/api/projects/123/deploy \
  -H "Authorization: Bearer $TEMPS_DEPLOY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"branch": "main", "commit": "abc123"}'
```

**Validate**: After setting up auto-deploy, push a test commit and confirm a new deployment appears in the dashboard within 60 seconds.

## Rollbacks

```bash
temps deployments list
temps rollback --deployment-id 456   # Specific deployment
temps rollback --previous            # Previous deployment
```

**Validate**: After rollback, verify the deployment URL serves the expected version. Check `temps deployments list` to confirm the active deployment ID changed.

## Health Checks

Configure in `temps.json`:

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

| Symptom | Check |
|---------|-------|
| Build fails | Build logs in dashboard, `buildCommand` correctness, missing dependencies |
| Container won't start | `PORT` env var usage, health check endpoint, container logs |
| Deployment stuck | Resource limits, local Docker build, deployment logs |
