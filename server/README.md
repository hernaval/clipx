# ClipX Server - Sync Broker

Rust server that acts as the central sync broker for ClipX. Handles authentication, clip ingestion, real-time delivery via WebSocket and SSE, device management, and audit logging.

## Responsibilities

- Authenticate devices via JWT
- Accept clip uploads with idempotency (deduplicate by `clip_id`)
- Publish clips to Redis pub/sub for distribution
- Relay clips to connected WebSocket clients (desktop) and SSE subscribers (browser)
- Manage device pairing and revocation
- Record audit logs for all actions
- Persist data in SQLite (dev) or Postgres (production)

## Build and Run

```bash
cd server
cargo build --release
cargo run
```

## Configuration

Via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `sqlite:clipx-server.db?mode=rwc` | Database connection URL |
| `JWT_SECRET` | `clipx-dev-secret-change-in-production!` | JWT signing secret |
| `REDIS_URL` | (none) | Redis URL for pub/sub relay |
| `BIND_ADDR` | `0.0.0.0:8080` | Server bind address |

## API Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/v1/clips` | JWT | Upload a clip |
| GET | `/api/v1/clips` | JWT | Get recent clips |
| POST | `/api/v1/pair/request` | JWT | Generate pairing token |
| POST | `/api/v1/pair/confirm` | none | Confirm pairing with token |
| GET | `/api/v1/devices` | JWT | List paired devices |
| DELETE | `/api/v1/devices/{id}` | JWT | Revoke a device |
| GET | `/api/v1/audit-logs` | JWT | Get audit log entries |
| GET | `/api/v1/ws?token=<JWT>` | query | WebSocket connection |
| GET | `/api/v1/sse?token=<JWT>` | query | SSE event stream |
| POST | `/api/v1/bootstrap` | none | Create user + device (dev) |

See `docs/openapi.yaml` for full API specification.

## Example Commands

### Bootstrap (create user and get JWT)

```bash
curl -X POST http://localhost:8080/api/v1/bootstrap \
  -H "Content-Type: application/json" \
  -d '{"email":"user@example.com","device_name":"My PC"}'
```

### Upload a clip

```bash
curl -X POST http://localhost:8080/api/v1/clips \
  -H "Authorization: Bearer <JWT>" \
  -H "Content-Type: application/json" \
  -d '{
    "device_id":"<device_id>",
    "clip_id":"'"$(uuidgen)"'",
    "timestamp":"2026-05-29T23:16:00Z",
    "format":"text",
    "content":"Hello world",
    "e2e": false
  }'
```

### Subscribe to SSE

```bash
curl -N "http://localhost:8080/api/v1/sse?token=<JWT>"
```

### Pair a new device

```bash
# Step 1: Request token (from existing device)
curl -X POST http://localhost:8080/api/v1/pair/request \
  -H "Authorization: Bearer <JWT>"

# Step 2: Confirm (from new device)
curl -X POST http://localhost:8080/api/v1/pair/confirm \
  -H "Content-Type: application/json" \
  -d '{"token":"<pairing_token>","device_name":"New Device","public_key":null}'
```

### Revoke a device

```bash
curl -X DELETE http://localhost:8080/api/v1/devices/<device_id> \
  -H "Authorization: Bearer <JWT>"
```

## Security

- TLS via `rustls` (configure reverse proxy for production)
- JWT tokens signed with HS256, 24-hour expiry
- Rate limiting recommended via reverse proxy
- Device revocation prevents future API access
- Pairing tokens expire after 10 minutes, single-use

## Testing

```bash
cargo test
```

Tests cover JWT roundtrip, database CRUD, clip idempotency, audit logs, pairing tokens, and pub/sub relay.
