# OpenTelemetry + error-tracking demo for Temps multi-node.
# Same image runs as gateway or backend via the ROLE env var.
FROM node:20-slim

WORKDIR /app

# Install deps first for layer caching.
COPY package.json ./
RUN npm install --omit=dev --no-audit --no-fund

COPY tracing.js server.js ./

ENV PORT=8080
EXPOSE 8080

# `-r ./tracing.js` boots the OTel SDK before the app loads `http`.
CMD ["node", "-r", "./tracing.js", "server.js"]
