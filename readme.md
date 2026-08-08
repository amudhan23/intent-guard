# req-guard

An async HTTP proxy in Rust that validates incoming requests against configurable rules before forwarding them to a backend — rejecting malicious payloads, oversized bodies, and requests to sensitive paths, while transparently relaying legitimate traffic.

Built with Tokio and Hyper. Extends [`payload-guard`](https://github.com/amudhan23/payload-guard) (a CLI request scanner) into a live, running proxy.

## What it does

- Receives HTTP requests (any method, any path)
- Checks the body against configurable suspicious-pattern rules (SQL injection, XSS, path traversal)
- Checks the method + path against configurable privilege-escalation rules
- Rejects flagged requests with the appropriate status code and reason — before any backend is ever contacted
- Forwards clean requests to a real backend and relays the backend's actual response

## Why

Explores the pattern of validating agent/API traffic at the network boundary — checking not just "is this request authenticated" but "does the content of this request look dangerous" — before it reaches a real system.

## Running it

```bash
cargo run
```

Server listens on `localhost:3000`. Rules are loaded from `rules.yaml` at startup.

## Example usage

**Clean request — forwarded to the real backend:**
```bash
curl -i -X POST localhost:3000/ -d "hello world"
```
Returns the actual response from the configured backend (currently `httpbin.org`).

**Malicious pattern — blocked before forwarding:**
```bash
curl -i -X POST localhost:3000/ -d "'; DROP TABLE users; --"
```
```
HTTP/1.1 403 Forbidden
Privileged error
```

**Oversized body — blocked before forwarding:**
```bash
curl -i -X POST localhost:3000/ -d "$(python3 -c 'print("a" * 70000)')"
```
```
HTTP/1.1 413 Payload Too Large
Body too big
```

**Privilege-escalation path — blocked before forwarding:**
```bash
curl -i -X PUT localhost:3000/admin/users/role
```
```
HTTP/1.1 403 Forbidden
Privileged error
```

**Clean GET — forwarded, real backend response relayed:**
```bash
curl -i localhost:3000/ip
```
```json
{
  "origin": "..."
}
```

## Configuration

`rules.yaml`:
```yaml
suspicious_body_patterns:
  - "DROP TABLE"
  - "; --"
  - "<script>"
  - "../../"

risky_privilege_paths:
  - "/admin/"
```

Editing the file and restarting the server changes behavior — no recompilation needed.

## Architecture

- `src/main.rs` — server bootstrap, accept loop, per-connection task spawning
- `src/handler.rs` (or wherever `echo`/routing lives) — request routing, validation orchestration, forwarding dispatch
- `src/scanner.rs` — pattern-matching logic (reused from `payload-guard`)
- `src/rules.rs` — YAML-loaded rule structs
- Shared rules config via `Arc<Rules>`, cloned cheaply per connection — no mutex needed since rules are read-only after startup

## Design decisions worth noting

- **Body buffered, not streamed** — request bodies are fully collected before validation, since rule-checking requires the complete content. A size limit (64KB) protects against unbounded memory use from oversized payloads.
- **Generic forwarding function** — `forward_to_backend` is generic over the request body type (`Body` trait bound), so it works uniformly whether the body is a live incoming stream (unread, e.g. PUT) or a freshly reconstructed buffer (e.g. POST, after validation already consumed the original body).
- **Errors converted to responses, not propagated** — forwarding failures return a `502 Bad Gateway` with a logged reason, rather than crashing the connection.

## Known limitations (not production-ready)

- No timeout on backend connections — a hanging backend would hang the request indefinitely
- No connection pooling — every request opens a fresh TCP connection to the backend
- No graceful shutdown
- Backend target is hardcoded, not configurable per-route
- No authentication, rate limiting, or TLS
- Rules are loaded once at startup — no hot-reload

## Testing

```bash
cargo test
```

Unit tests cover the scanning logic (`scan_body`, `scan_privilege`) against known-malicious and known-clean inputs.
