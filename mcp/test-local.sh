#!/bin/bash

# Test script for Temps MCP Server
# This script demonstrates how to test the server manually

echo "Testing Temps MCP Server"
echo "========================"
echo ""

# Start the server in background
echo "Starting MCP server..."
node dist/index.js &
SERVER_PID=$!

# Wait a moment for server to start
sleep 1

echo "Server started with PID: $SERVER_PID"
echo ""
echo "Testing tools/list..."
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | node dist/index.js &
sleep 1

echo ""
echo "To test interactively, use MCP Inspector:"
echo "  npm install -g @modelcontextprotocol/inspector"
echo "  mcp-inspector node dist/index.js"
echo ""
echo "Or add to Claude Desktop config:"
echo '  "temps": {"command": "node", "args": ["'$(pwd)'/dist/index.js"]}'
echo ""

# Kill background server
kill $SERVER_PID 2>/dev/null
