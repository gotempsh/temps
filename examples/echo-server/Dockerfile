# Zero-dependency echo server — builds in seconds (no npm install needed).
FROM node:22-alpine

WORKDIR /app
COPY package.json server.js ./

ENV PORT=8080
EXPOSE 8080
USER node

CMD ["node", "server.js"]
