# HTTP Authentication Troubleshooting Guide

This guide helps you diagnose authentication problems when Jossie makes HTTP requests to external services using the `http_request` tool.

## Quick Start

Run Jossie with debug logging enabled:

```bash
RUST_LOG=jossie_integration_http=debug cargo run
```

Or for even more detail:

```bash
RUST_LOG=debug cargo run
```

## Log Levels for Authentication Issues

The http_request integration now logs authentication details at different levels:

- **`ERROR`** - Critical auth failures (401, 403, request failures)
- **`WARN`** - Auth header stripping, WWW-Authenticate challenges, domain blocking
- **`INFO`** - Request/response status, auth header presence
- **`DEBUG`** - All headers (redacted), body types, redirects

## Common Authentication Problems & Solutions

### Problem 1: "Authentication failed: HTTP 401 Unauthorized"

**What the logs will show:**
```
ERROR Authentication failed: HTTP 401 Unauthorized from https://api.example.com
WARN Response includes WWW-Authenticate header (auth challenge): Bearer realm="..."
```

**Possible Causes:**
1. **No auth header sent**
   - Look for: `DEBUG Request does NOT include Authorization header`
   - Solution: Ensure Jossie is passing the auth header in the tool call

2. **Wrong credentials**
   - Look for: `INFO Sending request WITH Authorization header to https://...`
   - Solution: Verify the token/credentials are correct

3. **Expired token**
   - Check the response body for error messages about expiration
   - Solution: Refresh the token

### Problem 2: "Authorization failed: HTTP 403 Forbidden"

**What the logs will show:**
```
ERROR Authorization failed: HTTP 403 Forbidden from https://api.example.com
INFO Sending request WITH Authorization header to https://...
```

**Possible Causes:**
1. **Valid auth but insufficient permissions**
   - The credentials are accepted but don't have access to the resource
   - Solution: Check API permissions/scopes

2. **Domain/IP restrictions**
   - Some APIs restrict by IP address
   - Check the response body for details

### Problem 3: "Authentication header present but domain '...' is not in allowed_domains list"

**What the logs will show:**
```
WARN Blocked: Authentication header present but domain 'untrusted.com' is not in allowed_domains list
```

**Solution:**
Add the domain to the `allowed_domains` list in your `config.toml`:

```toml
[integrations.http]
allowed_domains = ["api.example.com", "trusted-api.com"]
```

Or use `["*"]` to allow all domains (not recommended for production).

### Problem 4: Auth works initially but fails after redirect

**What the logs will show:**
```
WARN Redirecting cross-origin from https://api1.com to https://api2.com. Stripping sensitive headers.
WARN Stripped Authorization header due to cross-origin redirect
ERROR Authentication failed: HTTP 401 Unauthorized from https://api2.com
```

**Cause:**
For security, auth headers are automatically stripped during cross-origin redirects.

**Solutions:**
1. Use the final URL directly (avoid redirects)
2. Set `follow_redirects: false` and handle redirects manually
3. If the redirect is same-origin, auth headers will be preserved

### Problem 5: Header parsing failures

**What the logs will show:**
```
WARN Failed to parse header: Content-Type = invalid/value/here
```

**Cause:**
Invalid header name or value format.

**Solution:**
- Header names must be valid HTTP header names
- Header values must be ASCII text without control characters
- Check for special characters that need escaping

### Problem 6: SSRF Protection Blocking Request

**What the logs will show:**
```
WARN SSRF protection: Blocked request to non-globally-reachable URL: http://10.0.0.5
```

**Cause:**
The URL targets a private/local network (localhost, 10.x, 192.168.x, etc.)

**Solution:**
This is a security feature. Only globally-reachable URLs are allowed (except in test mode).

## Advanced Triaging Techniques

### 1. Check if auth headers are being sent

Look for this sequence in logs:
```
DEBUG Request includes Authorization header
INFO Sending request WITH Authorization header to https://api.example.com
```

If missing:
```
DEBUG Request does NOT include Authorization header
```

### 2. Trace redirect behavior

For same-origin redirects:
```
DEBUG Redirecting same-origin from https://api.com/v1 to https://api.com/v2. Preserving headers.
```

For cross-origin redirects (auth is stripped):
```
WARN Redirecting cross-origin from https://api1.com to https://api2.com. Stripping sensitive headers.
WARN Stripped Authorization header due to cross-origin redirect
```

### 3. Check response for auth challenges

```
WARN Response includes WWW-Authenticate header (auth challenge): Bearer realm="api", error="invalid_token"
```

This tells you:
- The server expects authentication
- What type (Bearer, Basic, etc.)
- Why it failed (invalid_token, expired, etc.)

### 4. Monitor request/response lifecycle

A successful auth flow looks like:
```
INFO Starting HTTP request: GET https://api.example.com
DEBUG Request includes Authorization header
INFO Sending request WITH Authorization header to https://api.example.com
INFO Received response: 200 OK from GET https://api.example.com
INFO HTTP request completed successfully: GET https://api.example.com
```

## Example Log Output

Here's what a complete authentication failure looks like:

```
[2026-02-01T14:30:00Z INFO  jossie_integration_http] Starting HTTP request: GET https://api.github.com/user
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] Request params - timeout_ms: Some(20000), follow_redirects: false, has_headers: true, has_query: false
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] URL passed SSRF validation: https://api.github.com/user
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] Request includes Authorization header
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] Request has no body
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] Domain 'api.github.com' is in allowed_domains list for auth
[2026-02-01T14:30:00Z INFO  jossie_integration_http] HTTP GET https://api.github.com/user
[2026-02-01T14:30:00Z DEBUG jossie_integration_http] Request headers: {"authorization": "[REDACTED]", "user-agent": "reqwest/0.12.28"}
[2026-02-01T14:30:00Z INFO  jossie_integration_http] Sending request WITH Authorization header to https://api.github.com/user
[2026-02-01T14:30:01Z INFO  jossie_integration_http] Received response: 401 Unauthorized from GET https://api.github.com/user
[2026-02-01T14:30:01Z ERROR jossie_integration_http] Authentication failed: HTTP 401 Unauthorized from https://api.github.com/user
[2026-02-01T14:30:01Z WARN  jossie_integration_http] Response includes WWW-Authenticate header (auth challenge): Bearer realm="GitHub API"
```

## Configuration Tips

### Enable comprehensive HTTP logging in config.toml

```toml
[integrations.http]
allowed_domains = ["*"]  # Or list specific domains
# Note: Empty array means allow all domains
```

### Set appropriate log levels

In your environment or config:
```bash
# Minimum for auth troubleshooting
RUST_LOG=jossie_integration_http=info

# For detailed debugging
RUST_LOG=jossie_integration_http=debug

# Everything
RUST_LOG=debug
```

## Getting Help

When reporting authentication issues, include:

1. **The exact error message** from Jossie
2. **Relevant log output** (with timestamps)
3. **The target API/service** you're trying to reach
4. **Whether it works in curl/Postman** with the same credentials
5. **Any recent changes** to credentials or API configuration
