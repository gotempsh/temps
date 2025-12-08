# Dockerfile that builds successfully but fails health checks
# The container starts but doesn't listen on any port

FROM alpine:latest

# Install bash for better script support
RUN apk add --no-cache bash

# Create a script that runs but doesn't start a web server
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Expose port (but won't actually listen on it)
EXPOSE 3000

# Run the script that just sleeps
ENTRYPOINT ["/entrypoint.sh"]
