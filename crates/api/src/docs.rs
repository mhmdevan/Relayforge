use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

pub async fn openapi_json() -> impl IntoResponse {
    (StatusCode::OK, Json(openapi_spec()))
}

pub async fn swagger_ui() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Relayforge API Docs</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
    <style>
      body { margin: 0; background: #fafafa; }
      .topbar { display: none; }
    </style>
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
    <script>
      window.ui = SwaggerUIBundle({
        url: '/openapi.json',
        dom_id: '#swagger-ui',
        deepLinking: true,
        displayRequestDuration: true,
        presets: [SwaggerUIBundle.presets.apis],
      });
    </script>
  </body>
</html>
"#,
    )
}

pub async fn docs_redirect() -> Response {
    let mut response = Response::new("".into());
    *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/docs/"));
    response
}

fn openapi_spec() -> Value {
    json!({
      "openapi": "3.0.3",
      "info": {
        "title": "Relayforge API",
        "version": "0.1.0",
        "description": "Webhook ingestion and job orchestration API"
      },
      "servers": [
        { "url": "http://127.0.0.1:8080" }
      ],
      "components": {
        "securitySchemes": {
          "bearerAuth": {
            "type": "http",
            "scheme": "bearer",
            "bearerFormat": "API Key"
          }
        },
        "schemas": {
          "WebhookAccepted": {
            "type": "object",
            "properties": {
              "inbox_id": { "type": "string", "format": "uuid" },
              "signature_valid": { "type": "boolean" },
              "accepted": { "type": "boolean" },
              "job_id": { "type": ["string", "null"], "format": "uuid" },
              "job_created": { "type": ["boolean", "null"] },
              "deduped": { "type": ["boolean", "null"] }
            },
            "required": ["inbox_id", "signature_valid", "accepted"]
          },
          "JobView": {
            "type": "object",
            "properties": {
              "id": { "type": "string", "format": "uuid" },
              "tenant_id": { "type": "string", "format": "uuid" },
              "source": { "type": "string" },
              "status": { "type": "string" },
              "attempts": { "type": "integer" },
              "last_error": { "type": ["string", "null"] },
              "created_at": { "type": "string", "format": "date-time" },
              "updated_at": { "type": "string", "format": "date-time" },
              "first_inbox_id": { "type": "string", "format": "uuid" },
              "last_inbox_id": { "type": "string", "format": "uuid" }
            },
            "required": ["id", "tenant_id", "source", "status", "attempts", "created_at", "updated_at", "first_inbox_id", "last_inbox_id"]
          },
          "ReprocessAccepted": {
            "type": "object",
            "properties": {
              "job_id": { "type": "string", "format": "uuid" },
              "outbox_id": { "type": "string", "format": "uuid" },
              "reprocessed": { "type": "boolean" }
            },
            "required": ["job_id", "outbox_id", "reprocessed"]
          },
          "ErrorBody": {
            "type": "object",
            "properties": {
              "error": { "type": "string" }
            },
            "required": ["error"]
          }
        }
      },
      "paths": {
        "/healthz": {
          "get": {
            "summary": "Liveness probe",
            "responses": {
              "200": { "description": "Healthy" }
            }
          }
        },
        "/readyz": {
          "get": {
            "summary": "Readiness probe",
            "responses": {
              "200": { "description": "Ready" },
              "503": { "description": "Not ready" }
            }
          }
        },
        "/metrics": {
          "get": {
            "summary": "Prometheus metrics",
            "responses": {
              "200": { "description": "Metrics in text format" }
            }
          }
        },
        "/webhooks/{source}": {
          "post": {
            "summary": "Ingest provider webhook",
            "parameters": [
              {
                "name": "source",
                "in": "path",
                "required": true,
                "schema": { "type": "string", "enum": ["generic", "github", "stripe"] }
              },
              {
                "name": "traceparent",
                "in": "header",
                "required": false,
                "schema": { "type": "string" },
                "description": "W3C traceparent header for distributed tracing propagation"
              },
              {
                "name": "tracestate",
                "in": "header",
                "required": false,
                "schema": { "type": "string" },
                "description": "W3C tracestate header for distributed tracing propagation"
              }
            ],
            "requestBody": {
              "required": true,
              "content": {
                "application/json": {
                  "schema": { "type": "object" }
                }
              }
            },
            "responses": {
              "202": {
                "description": "Webhook accepted",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/WebhookAccepted" } } }
              },
              "400": {
                "description": "Validation error",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              },
              "401": {
                "description": "Signature invalid",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              },
              "409": {
                "description": "Conflict (e.g., Stripe replay detected)",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              },
              "429": {
                "description": "Tenant rate limit or quota exceeded",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              }
            }
          }
        },
        "/jobs/{id}": {
          "get": {
            "summary": "Get job details",
            "parameters": [
              {
                "name": "id",
                "in": "path",
                "required": true,
                "schema": { "type": "string", "format": "uuid" }
              },
              {
                "name": "traceparent",
                "in": "header",
                "required": false,
                "schema": { "type": "string" },
                "description": "W3C traceparent header for distributed tracing propagation"
              },
              {
                "name": "tracestate",
                "in": "header",
                "required": false,
                "schema": { "type": "string" },
                "description": "W3C tracestate header for distributed tracing propagation"
              }
            ],
            "responses": {
              "200": {
                "description": "Job view",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JobView" } } }
              },
              "404": {
                "description": "Not found",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              }
            }
          }
        },
        "/admin/jobs/{id}/reprocess": {
          "post": {
            "summary": "Reprocess dead-lettered job",
            "security": [
              { "bearerAuth": [] }
            ],
            "parameters": [
              {
                "name": "id",
                "in": "path",
                "required": true,
                "schema": { "type": "string", "format": "uuid" }
              }
            ],
            "responses": {
              "202": {
                "description": "Reprocess accepted",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReprocessAccepted" } } }
              },
              "401": {
                "description": "Unauthorized",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              },
              "403": {
                "description": "Forbidden",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              },
              "409": {
                "description": "Invalid state",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorBody" } } }
              }
            }
          }
        }
      }
    })
}
