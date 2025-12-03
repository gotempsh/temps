#!/usr/bin/env node
/**
 * Script to add email endpoints to the OpenAPI schema
 */

const fs = require('fs');
const path = require('path');

const openapiPath = path.join(__dirname, 'openapi.json');

// Read current OpenAPI spec
const openapi = JSON.parse(fs.readFileSync(openapiPath, 'utf8'));

// Email endpoints paths
const emailPaths = {
  "/api/email-providers": {
    "post": {
      "tags": ["Email Providers"],
      "summary": "Create a new email provider",
      "operationId": "create_email_provider",
      "requestBody": {
        "content": {
          "application/json": {
            "schema": {
              "$ref": "#/components/schemas/CreateEmailProviderRequest"
            }
          }
        },
        "required": true
      },
      "responses": {
        "201": {
          "description": "Provider created successfully",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailProviderResponse"
              }
            }
          }
        },
        "400": { "description": "Invalid request" },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    },
    "get": {
      "tags": ["Email Providers"],
      "summary": "List all email providers",
      "operationId": "list_email_providers",
      "responses": {
        "200": {
          "description": "List of email providers",
          "content": {
            "application/json": {
              "schema": {
                "type": "array",
                "items": {
                  "$ref": "#/components/schemas/EmailProviderResponse"
                }
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/email-providers/{id}": {
    "get": {
      "tags": ["Email Providers"],
      "summary": "Get an email provider by ID",
      "operationId": "get_email_provider",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Provider ID",
          "required": true,
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "200": {
          "description": "Email provider details",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailProviderResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Provider not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    },
    "delete": {
      "tags": ["Email Providers"],
      "summary": "Delete an email provider",
      "operationId": "delete_email_provider",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Provider ID",
          "required": true,
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "204": { "description": "Provider deleted" },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Provider not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/email-domains": {
    "post": {
      "tags": ["Email Domains"],
      "summary": "Create a new email domain",
      "operationId": "create_email_domain",
      "requestBody": {
        "content": {
          "application/json": {
            "schema": {
              "$ref": "#/components/schemas/CreateEmailDomainRequest"
            }
          }
        },
        "required": true
      },
      "responses": {
        "201": {
          "description": "Domain created successfully",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailDomainWithDnsResponse"
              }
            }
          }
        },
        "400": { "description": "Invalid request" },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    },
    "get": {
      "tags": ["Email Domains"],
      "summary": "List all email domains",
      "operationId": "list_email_domains",
      "responses": {
        "200": {
          "description": "List of email domains",
          "content": {
            "application/json": {
              "schema": {
                "type": "array",
                "items": {
                  "$ref": "#/components/schemas/EmailDomainResponse"
                }
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/email-domains/{id}": {
    "get": {
      "tags": ["Email Domains"],
      "summary": "Get an email domain by ID with DNS records",
      "operationId": "get_email_domain",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Domain ID",
          "required": true,
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "200": {
          "description": "Email domain details with DNS records",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailDomainWithDnsResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Domain not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    },
    "delete": {
      "tags": ["Email Domains"],
      "summary": "Delete an email domain",
      "operationId": "delete_email_domain",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Domain ID",
          "required": true,
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "204": { "description": "Domain deleted" },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Domain not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/email-domains/{id}/verify": {
    "post": {
      "tags": ["Email Domains"],
      "summary": "Verify an email domain's DNS configuration",
      "operationId": "verify_email_domain",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Domain ID",
          "required": true,
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "200": {
          "description": "Domain verification result",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailDomainResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Domain not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/emails": {
    "post": {
      "tags": ["Emails"],
      "summary": "Send an email",
      "operationId": "send_email",
      "requestBody": {
        "content": {
          "application/json": {
            "schema": {
              "$ref": "#/components/schemas/SendEmailRequest"
            }
          }
        },
        "required": true
      },
      "responses": {
        "201": {
          "description": "Email sent successfully",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/SendEmailResponse"
              }
            }
          }
        },
        "400": { "description": "Invalid request or domain not verified" },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    },
    "get": {
      "tags": ["Emails"],
      "summary": "List emails with optional filtering",
      "operationId": "list_emails",
      "parameters": [
        {
          "name": "domain_id",
          "in": "query",
          "description": "Filter by domain ID",
          "schema": { "type": "integer", "format": "int32" }
        },
        {
          "name": "project_id",
          "in": "query",
          "description": "Filter by project ID",
          "schema": { "type": "integer", "format": "int32" }
        },
        {
          "name": "status",
          "in": "query",
          "description": "Filter by status (queued, sent, failed, captured)",
          "schema": { "type": "string" }
        },
        {
          "name": "from_address",
          "in": "query",
          "description": "Filter by sender address",
          "schema": { "type": "string" }
        },
        {
          "name": "page",
          "in": "query",
          "description": "Page number",
          "schema": { "type": "integer", "format": "int64", "default": 1 }
        },
        {
          "name": "page_size",
          "in": "query",
          "description": "Page size",
          "schema": { "type": "integer", "format": "int64", "default": 20 }
        }
      ],
      "responses": {
        "200": {
          "description": "List of emails",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/PaginatedEmailsResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/emails/{id}": {
    "get": {
      "tags": ["Emails"],
      "summary": "Get an email by ID",
      "operationId": "get_email",
      "parameters": [
        {
          "name": "id",
          "in": "path",
          "description": "Email ID (UUID)",
          "required": true,
          "schema": { "type": "string" }
        }
      ],
      "responses": {
        "200": {
          "description": "Email details",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "404": { "description": "Email not found" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  },
  "/api/emails/stats": {
    "get": {
      "tags": ["Emails"],
      "summary": "Get email statistics",
      "operationId": "get_email_stats",
      "parameters": [
        {
          "name": "domain_id",
          "in": "query",
          "description": "Optional domain ID to filter stats",
          "schema": { "type": "integer", "format": "int32" }
        }
      ],
      "responses": {
        "200": {
          "description": "Email statistics",
          "content": {
            "application/json": {
              "schema": {
                "$ref": "#/components/schemas/EmailStatsResponse"
              }
            }
          }
        },
        "401": { "description": "Unauthorized" },
        "403": { "description": "Insufficient permissions" },
        "500": { "description": "Internal server error" }
      },
      "security": [{ "bearer_auth": [] }]
    }
  }
};

// Email schemas
const emailSchemas = {
  "EmailProviderType": {
    "type": "string",
    "enum": ["ses", "scaleway"],
    "description": "Email provider type"
  },
  "SesCredentials": {
    "type": "object",
    "required": ["access_key_id", "secret_access_key"],
    "properties": {
      "access_key_id": {
        "type": "string",
        "description": "AWS Access Key ID",
        "example": "AKIAIOSFODNN7EXAMPLE"
      },
      "secret_access_key": {
        "type": "string",
        "description": "AWS Secret Access Key",
        "example": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
      }
    }
  },
  "ScalewayCredentials": {
    "type": "object",
    "required": ["api_key", "project_id"],
    "properties": {
      "api_key": {
        "type": "string",
        "description": "Scaleway API Key",
        "example": "scw-secret-key-12345"
      },
      "project_id": {
        "type": "string",
        "description": "Scaleway Project ID",
        "example": "12345678-1234-1234-1234-123456789012"
      }
    }
  },
  "CreateEmailProviderRequest": {
    "type": "object",
    "required": ["name", "provider_type", "region"],
    "properties": {
      "name": {
        "type": "string",
        "description": "User-friendly name for the provider",
        "example": "My AWS SES"
      },
      "provider_type": {
        "$ref": "#/components/schemas/EmailProviderType"
      },
      "region": {
        "type": "string",
        "description": "Cloud region",
        "example": "us-east-1"
      },
      "ses_credentials": {
        "$ref": "#/components/schemas/SesCredentials"
      },
      "scaleway_credentials": {
        "$ref": "#/components/schemas/ScalewayCredentials"
      }
    }
  },
  "EmailProviderResponse": {
    "type": "object",
    "required": ["id", "name", "provider_type", "region", "is_active", "credentials", "created_at", "updated_at"],
    "properties": {
      "id": {
        "type": "integer",
        "format": "int32"
      },
      "name": {
        "type": "string",
        "example": "My AWS SES"
      },
      "provider_type": {
        "$ref": "#/components/schemas/EmailProviderType"
      },
      "region": {
        "type": "string",
        "example": "us-east-1"
      },
      "is_active": {
        "type": "boolean"
      },
      "credentials": {
        "type": "object",
        "description": "Masked credentials for display"
      },
      "created_at": {
        "type": "string",
        "format": "date-time",
        "example": "2025-12-03T10:30:00Z"
      },
      "updated_at": {
        "type": "string",
        "format": "date-time",
        "example": "2025-12-03T10:30:00Z"
      }
    }
  },
  "CreateEmailDomainRequest": {
    "type": "object",
    "required": ["provider_id", "domain"],
    "properties": {
      "provider_id": {
        "type": "integer",
        "format": "int32",
        "description": "Provider ID to use for this domain"
      },
      "domain": {
        "type": "string",
        "description": "Domain name",
        "example": "updates.example.com"
      }
    }
  },
  "DnsRecordResponse": {
    "type": "object",
    "required": ["record_type", "name", "value"],
    "properties": {
      "record_type": {
        "type": "string",
        "description": "Record type: TXT, CNAME, MX",
        "example": "TXT"
      },
      "name": {
        "type": "string",
        "description": "DNS record name (host)",
        "example": "temps._domainkey.example.com"
      },
      "value": {
        "type": "string",
        "description": "DNS record value",
        "example": "v=DKIM1; k=rsa; p=MIGfMA0GCSqGSIb3..."
      },
      "priority": {
        "type": "integer",
        "format": "int32",
        "description": "Priority (for MX records)",
        "example": 10
      }
    }
  },
  "EmailDomainResponse": {
    "type": "object",
    "required": ["id", "provider_id", "domain", "status", "created_at", "updated_at"],
    "properties": {
      "id": {
        "type": "integer",
        "format": "int32"
      },
      "provider_id": {
        "type": "integer",
        "format": "int32"
      },
      "domain": {
        "type": "string",
        "example": "updates.example.com"
      },
      "status": {
        "type": "string",
        "example": "verified"
      },
      "last_verified_at": {
        "type": "string",
        "format": "date-time"
      },
      "verification_error": {
        "type": "string"
      },
      "created_at": {
        "type": "string",
        "format": "date-time",
        "example": "2025-12-03T10:30:00Z"
      },
      "updated_at": {
        "type": "string",
        "format": "date-time",
        "example": "2025-12-03T10:30:00Z"
      }
    }
  },
  "EmailDomainWithDnsResponse": {
    "type": "object",
    "required": ["domain", "dns_records"],
    "properties": {
      "domain": {
        "$ref": "#/components/schemas/EmailDomainResponse"
      },
      "dns_records": {
        "type": "array",
        "items": {
          "$ref": "#/components/schemas/DnsRecordResponse"
        }
      }
    }
  },
  "SendEmailRequest": {
    "type": "object",
    "required": ["domain_id", "from", "to", "subject"],
    "properties": {
      "domain_id": {
        "type": "integer",
        "format": "int32",
        "description": "Domain ID to send from"
      },
      "project_id": {
        "type": "integer",
        "format": "int32",
        "description": "Optional project ID for tracking"
      },
      "from": {
        "type": "string",
        "description": "Sender email address",
        "example": "hello@updates.example.com"
      },
      "from_name": {
        "type": "string",
        "description": "Sender display name",
        "example": "My App"
      },
      "to": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Recipient email addresses",
        "example": ["user@example.com"]
      },
      "cc": {
        "type": "array",
        "items": { "type": "string" },
        "description": "CC recipients"
      },
      "bcc": {
        "type": "array",
        "items": { "type": "string" },
        "description": "BCC recipients"
      },
      "reply_to": {
        "type": "string",
        "description": "Reply-to address"
      },
      "subject": {
        "type": "string",
        "description": "Email subject",
        "example": "Welcome to our platform!"
      },
      "html": {
        "type": "string",
        "description": "HTML body content",
        "example": "<h1>Hello World</h1>"
      },
      "text": {
        "type": "string",
        "description": "Plain text body content",
        "example": "Hello World"
      },
      "headers": {
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Custom headers"
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Tags for categorization",
        "example": ["welcome", "onboarding"]
      }
    }
  },
  "SendEmailResponse": {
    "type": "object",
    "required": ["id", "status"],
    "properties": {
      "id": {
        "type": "string",
        "description": "Email ID",
        "example": "550e8400-e29b-41d4-a716-446655440000"
      },
      "status": {
        "type": "string",
        "description": "Email status",
        "example": "sent"
      },
      "provider_message_id": {
        "type": "string",
        "description": "Provider message ID"
      }
    }
  },
  "EmailResponse": {
    "type": "object",
    "required": ["id", "domain_id", "from_address", "to_addresses", "subject", "status", "created_at"],
    "properties": {
      "id": {
        "type": "string",
        "example": "550e8400-e29b-41d4-a716-446655440000"
      },
      "domain_id": {
        "type": "integer",
        "format": "int32"
      },
      "project_id": {
        "type": "integer",
        "format": "int32"
      },
      "from_address": {
        "type": "string",
        "example": "hello@updates.example.com"
      },
      "from_name": {
        "type": "string"
      },
      "to_addresses": {
        "type": "array",
        "items": { "type": "string" }
      },
      "cc_addresses": {
        "type": "array",
        "items": { "type": "string" }
      },
      "bcc_addresses": {
        "type": "array",
        "items": { "type": "string" }
      },
      "reply_to": {
        "type": "string"
      },
      "subject": {
        "type": "string"
      },
      "html_body": {
        "type": "string"
      },
      "text_body": {
        "type": "string"
      },
      "headers": {
        "type": "object",
        "additionalProperties": { "type": "string" }
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" }
      },
      "status": {
        "type": "string",
        "example": "sent"
      },
      "provider_message_id": {
        "type": "string"
      },
      "error_message": {
        "type": "string"
      },
      "sent_at": {
        "type": "string",
        "format": "date-time"
      },
      "created_at": {
        "type": "string",
        "format": "date-time",
        "example": "2025-12-03T10:30:00Z"
      }
    }
  },
  "EmailStatsResponse": {
    "type": "object",
    "required": ["total", "sent", "failed", "queued", "captured"],
    "properties": {
      "total": {
        "type": "integer",
        "format": "int64"
      },
      "sent": {
        "type": "integer",
        "format": "int64"
      },
      "failed": {
        "type": "integer",
        "format": "int64"
      },
      "queued": {
        "type": "integer",
        "format": "int64"
      },
      "captured": {
        "type": "integer",
        "format": "int64",
        "description": "Emails captured without sending (Mailhog mode - no provider configured)"
      }
    }
  },
  "PaginatedEmailsResponse": {
    "type": "object",
    "required": ["data", "total", "page", "page_size"],
    "properties": {
      "data": {
        "type": "array",
        "items": {
          "$ref": "#/components/schemas/EmailResponse"
        }
      },
      "total": {
        "type": "integer",
        "format": "int64"
      },
      "page": {
        "type": "integer",
        "format": "int64"
      },
      "page_size": {
        "type": "integer",
        "format": "int64"
      }
    }
  }
};

// Add paths
Object.assign(openapi.paths, emailPaths);

// Add schemas
Object.assign(openapi.components.schemas, emailSchemas);

// Add tags if not present
if (!openapi.tags) {
  openapi.tags = [];
}

const emailTags = [
  { name: "Email Providers", description: "Email provider management endpoints" },
  { name: "Email Domains", description: "Email domain management and verification" },
  { name: "Emails", description: "Email sending and retrieval" }
];

for (const tag of emailTags) {
  if (!openapi.tags.find(t => t.name === tag.name)) {
    openapi.tags.push(tag);
  }
}

// Write updated OpenAPI spec
fs.writeFileSync(openapiPath, JSON.stringify(openapi, null, '\t'), 'utf8');

console.log('Email endpoints added to OpenAPI schema successfully!');
console.log('Added paths:', Object.keys(emailPaths).length);
console.log('Added schemas:', Object.keys(emailSchemas).length);
