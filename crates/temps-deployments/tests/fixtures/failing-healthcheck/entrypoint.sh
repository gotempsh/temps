#!/bin/bash

# This script simulates an application that starts but fails to serve HTTP requests
# It will cause health check timeouts in the deployment job

echo "Application starting..."
echo "This container will NOT listen on any port"
echo "Health checks will fail, causing deployment timeout"

# Just sleep indefinitely without starting any server
# This simulates an app that crashes or hangs during startup
sleep infinity
