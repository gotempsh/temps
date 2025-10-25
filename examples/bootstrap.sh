#!/bin/bash

# Bootstrap script to generate example applications for testing Temps presets
# This script creates minimal working examples for each supported framework

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "🚀 Bootstrapping Temps example applications..."
echo ""

# Vite + React
echo "📦 Creating Vite + React example..."
cd "$SCRIPT_DIR/vite"
if [ ! -d "react-basic" ]; then
    bun create vite react-basic --template react
    cd react-basic
    bun install
    echo "✅ Vite + React example created"
else
    echo "⏭️  Vite React example already exists, skipping"
fi
cd "$SCRIPT_DIR"
echo ""

# Next.js (npm)
echo "📦 Creating Next.js example (npm)..."
cd "$SCRIPT_DIR/nextjs"
if [ ! -d "basic" ]; then
    # Use expect to automate interactive prompts
    npx --yes create-next-app@latest basic \
        --typescript \
        --tailwind \
        --eslint \
        --app \
        --src-dir \
        --import-alias "@/*" \
        --turbopack \
        --use-bun \
        --no-git \
        --skip-install
    cd basic
    bun install
    echo "✅ Next.js (npm) example created"
else
    echo "⏭️  Next.js example already exists, skipping"
fi
cd "$SCRIPT_DIR"
echo ""

# NestJS
echo "📦 Creating NestJS example (npm)..."
cd "$SCRIPT_DIR/nestjs"
if [ ! -d "basic" ]; then
    npx --yes @nestjs/cli@latest new basic \
        --package-manager npm \
        --language TS \
        --strict \
        --skip-git
    cd basic
    npm install
    echo "✅ NestJS (npm) example created"
else
    echo "⏭️  NestJS example already exists, skipping"
fi
cd "$SCRIPT_DIR"
echo ""

echo "✨ All examples created successfully!"
echo ""
echo "📁 Example structure:"
echo "   examples/"
echo "   ├── vite/react-basic         (Vite + React)"
echo "   ├── nextjs/basic             (Next.js + TypeScript + Tailwind)"
echo "   └── nestjs/basic             (NestJS + TypeScript)"
echo ""
echo "🧪 Run tests with:"
echo "   cargo test --test public_repo_deployment_test -- --nocapture"
