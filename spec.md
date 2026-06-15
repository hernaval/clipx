# ClipboardSync Phase 1 Specification

> **Project name**: ClipX  
> **Purpose**: Secure, maintainable clipboard sync system that captures text clipboard entries on Windows, syncs them quickly across devices, and provides an overlay picker and history UI. Phase 1 focuses on text only, hybrid real‑time sync, hotkey customization, browser popup history, device pairing, audit logs, and offline LAN sync.

---

## Architecture

**High level components**

| Component | Language | Responsibility |
|---|---:|---|
| **core** | C# .NET 7 | Windows clipboard bridge: detect copy events, read/write clipboard, expose JSON RPC over named pipe or local HTTP for the Rust client |
| **client** | Rust | Desktop app: IPC to core, local storage, sync engine, overlay UI, history UI, system tray, hotkey customization, offline queue, local mini server, encryption |
| **server** | Rust | Central sync broker: REST API, WebSocket, SSE, Redis pub/sub relay, device management, audit logs, persistence, TLS and JWT auth |
| **extension** | TypeScript | Browser extension Manifest V3: manual push, SSE subscription, popup history UI, pairing flow |
| **pubsub broker** | Redis | Message distribution for user topics in Phase 1 |

**Design principles**

- **Modularity**: clear separation between clipboard access, sync core, UI, and server.
- **SOLID**: single responsibility modules, dependency injection, trait interfaces for testability.
- **Security by default**: TLS, JWT, optional end‑to‑end encryption, device revocation.
- **Maintainability**: small crates/projects, documented public interfaces, readable code and tests.

---

## Component Responsibilities

### Core Clipboard Bridge
- **Primary role**: expose Windows clipboard events and read/write operations to the Rust client.
- **Key behaviors**
  - Detect copy events using `AddClipboardFormatListener`.
  - Emit events only for text formats.
  - Provide `get_clipboard` and `set_clipboard` operations.
  - Filter non‑text formats by default.
- **Transport**
  - Default: named pipe `\\.\pipe\clipboardsync` with JSON RPC messages.
  - Optional: local HTTP `http://127.0.0.1:PORT/` for debugging.
- **Security**
  - Restrict named pipe access to current user.
  - Minimal logging of clipboard content; prefer metadata logs.

### Client Desktop App
- **Primary role**: receive clipboard events, store history, sync with server and peers, present overlay and history UI, manage device pairing and settings.
- **Key modules**
  - `ipc` module: named pipe client to core; abstract `ClipboardProvider` trait.
  - `storage` module: SQLite via `sqlx` for `clips`, `devices`, `queue`, `settings`.
  - `sync` module: REST uploader, WebSocket listener, Redis pub/sub consumer logic, retry/backoff queue.
  - `crypto` module: optional E2E primitives and key management.
  - `ui` module: overlay and history UI via Tauri or egui.
  - `tray` module: system tray integration and quick actions.
  - `hotkeys` module: global hotkey registration and customization.
- **Behavior**
  - Accept clipboard events from core; ignore non‑text.
  - Create `clip_id` UUID and store clip locally with status `pending`.
  - Attempt upload to server via REST; if offline, attempt local peers; otherwise keep in queue.
  - Maintain WebSocket connection for inbound real‑time messages.
  - Provide overlay UI on hotkey to select and paste clips.
  - Provide system tray with quick actions and status.
- **Offline**
  - Persistent queue with exponential backoff and retry.
  - Optional local mini server `POST /local-sync` for LAN peer sync.
  - Manual peer discovery via QR or manual IP entry in Phase 1.
- **Observability**
  - Structured logs via `tracing`.
  - Local metrics endpoint for dev.

### Server
- **Primary role**: authenticate devices, accept clip uploads, publish to pub/sub, relay to clients via WebSocket and SSE, persist data, manage devices and audit logs.
- **Key modules**
  - REST API handlers for pairing, clips, devices.
  - WebSocket endpoint for desktop clients.
  - SSE endpoint for browser clients.
  - Redis pub/sub relay.
  - Persistence layer using `sqlx` with SQLite for dev and Postgres for production.
  - Auth module issuing JWTs and device tokens.
- **Behavior**
  - Validate JWT and idempotency by `clip_id` on ingestion.
  - Publish clip messages to Redis channel `clips:user:<user_id>`.
  - Relay messages to connected WebSocket clients and SSE subscribers.
  - Record audit logs for upload, delivery, pairing, and revocation.
- **Security**
  - TLS via `rustls`.
  - JWT signed with server private key.
  - Rate limiting and input validation.

### Browser Extension
- **Primary role**: lightweight capture and manual push inside browser, SSE subscription for incoming clips, popup history UI.
- **Behavior**
  - Manual push using `navigator.clipboard.readText()` and `POST /api/v1/clips`.
  - SSE subscription via `EventSource` to `GET /api/v1/sse?token=<JWT>`.
  - Popup UI for recent clips and copy action.
  - Offline queue in IndexedDB and retry logic.
  - Pairing via token or QR code scanned from desktop app.
- **Security**
  - Use HTTPS endpoints and WebCrypto for optional E2E.

---

## Functional Requirements

**Phase 1 must include**

- Hybrid sync model: REST upload client→server, Redis pub/sub broker, SSE for browsers, WebSocket for desktop.
- Clipboard capture on Windows for text only.
- Hotkey customization for overlay trigger.
- Overlay UI to select and paste synced clips.
- System tray with quick actions.
- Browser popup history with manual push and SSE subscription.
- Device pairing and management with ability to revoke devices.
- Audit logs for actions and deliveries.
- Offline queue and local peer sync via mini server.
- TLS transport and JWT authentication.
- Optional end‑to‑end encryption toggle.

---

## Technical Requirements

### Data Models

**users**
- `id UUID PRIMARY KEY`
- `email TEXT`
- `created_at TIMESTAMP`

**devices**
- `id UUID PRIMARY KEY`
- `user_id UUID REFERENCES users(id)`
- `name TEXT`
- `public_key TEXT`
- `paired_at TIMESTAMP`
- `revoked BOOLEAN`

**clips**
- `id UUID PRIMARY KEY`
- `user_id UUID`
- `device_id UUID`
- `content TEXT`
- `format TEXT`
- `e2e BOOLEAN`
- `created_at TIMESTAMP`

**audit_logs**
- `id UUID PRIMARY KEY`
- `user_id UUID`
- `device_id UUID`
- `action TEXT`
- `details JSONB`
- `timestamp TIMESTAMP`

**queue** (client local)
- `id UUID PRIMARY KEY`
- `clip_id UUID`
- `status TEXT`
- `attempts INT`
- `next_attempt_at TIMESTAMP`

### REST API Contracts

**Upload clip**
- `POST /api/v1/clips`
- Headers: `Authorization: Bearer <JWT>`
- Body example
```json
{
  "device_id": "uuid",
  "clip_id": "uuid",
  "timestamp": "2026-05-29T23:16:00Z",
  "format": "text",
  "content": "Hello world",
  "e2e": false
}
```
- Responses
  - `201 Created` `{ "status":"ok", "clip_id":"..." }`
  - `409 Conflict` duplicate `clip_id`
  - `401 Unauthorized` invalid JWT

**SSE**
- `GET /api/v1/sse?token=<JWT>`
- Event example
```
event: clip
data: {"user_id":"u-1","clip_id":"c-456","device_id":"d-123","timestamp":"...","content":"Hello world","e2e":false}
```

**WebSocket**
- `wss://server/api/v1/ws?token=<JWT>`
- Messages JSON with `type` field: `clip`, `ack`, `ping`, `error`.

**Pairing**
- `POST /api/v1/pair/request` returns pairing token.
- `POST /api/v1/pair/confirm` with token and device public key returns device token and initial JWT.

### Pub/Sub Message Format

Redis channel `clips:user:<user_id>` message
```json
{
  "type": "clip",
  "user_id": "u-1",
  "clip_id": "c-456",
  "device_id": "d-123",
  "timestamp": "2026-05-29T23:16:00Z",
  "content": "ciphertext-or-plaintext",
  "e2e": false
}
```

### Encryption and Key Management

- **Transport**: TLS via `rustls`.
- **Auth**: JWT for API calls; device tokens for pairing.
- **E2E optional**
  - Device generates X25519 or RSA key pair.
  - Upload public key during pairing.
  - Per clip: generate AES‑GCM key, encrypt clip, encrypt AES key for each recipient using ECDH or RSA.
  - Server stores ciphertext only; encrypted keys per device if server assists distribution.
  - Provide UI for key export/import and rotation.

### Offline and Local Peer Sync

- **Local mini server**
  - Runs on configurable port.
  - `POST /local-sync` accepts clip payloads with device token header.
  - Validates token and stores clip locally.
- **Discovery**
  - Phase 1: manual IP entry or QR code with `http://<ip>:<port>/pair?token=...`.
- **Queue behavior**
  - On server unreachable, attempt local peers; otherwise keep in queue and retry with exponential backoff.
- **Security**
  - Local endpoints require device token; recommend TLS on LAN or short‑lived tokens and user confirmation.

### Error Handling and Idempotency

- Server deduplicates by `clip_id`.
- Client uses exponential backoff with jitter; after N failures mark as `failed`.
- Structured JSON error responses with `code`, `message`, `details`.
- Audit logs record high level actions.

---

## Development Plan Phase 1 Milestones

1. **Milestone 1 Clipboard Bridge and Client Skeleton**
   - C# named pipe JSON RPC server emitting `clipboard_event`.
   - Rust client IPC module that logs events and stores clips in SQLite.
   - Unit tests for serialization and IPC.

2. **Milestone 2 Server Core and Auth**
   - Rust server with `POST /api/v1/clips`, JWT auth, pairing endpoints, Redis integration, SQLite persistence.
   - Integration tests for clip ingestion and pub/sub publish.

3. **Milestone 3 Real Time Delivery**
   - WebSocket endpoint for desktop clients and SSE for browser clients.
   - Client WebSocket subscription and server relay from Redis to sockets.
   - End‑to‑end test: client A upload → server publish → client B receive.

4. **Milestone 4 UI Tray and Hotkeys**
   - System tray with quick actions and hotkey customization.
   - Overlay UI for paste selection and paste injection.

5. **Milestone 5 Browser Extension and Pairing**
   - Extension popup with manual push, SSE subscription, history UI, pairing flow.
   - IndexedDB queue for offline pushes.

6. **Milestone 6 Device Management and Audit Logs**
   - Device list, revoke device, audit log viewer.
   - Tests for revoke flow and audit entries.

---

## Testing Strategy

- **Unit tests**: crypto primitives, storage layer, IPC serialization, REST handlers.
- **Integration tests**: server + Redis + DB in Docker compose; client integration tests using a test harness that simulates core events.
- **E2E tests**: simulate two clients and server; verify upload → publish → receive; verify offline queue and local peer sync.
- **Manual QA**: clipboard capture reliability across common apps, hotkey conflicts, overlay paste correctness.
- **Security tests**: TLS enforcement, JWT expiry, device revocation, E2E key exchange correctness.

---

## Deliverables for Implementation

1. **Scaffolded repositories** for `core`, `client`, `server`, `extension` with clear module boundaries.
2. **Automated tests** as described in testing strategy.
3. **Infra** folder with Docker compose for Redis and Postgres for local dev.
4. **Comprehensive README** for each component generated by the implementer. Each README must include:
   - Purpose and responsibilities
   - Build and run instructions
   - Configuration examples
   - API examples and sample payloads
   - Testing instructions
   - Security notes and pairing instructions
5. **OpenAPI skeleton** for server endpoints and `docs/` with sequence diagrams in Mermaid for copy, sync, and paste flows.
6. **SQLite schema migration SQL** for Phase 1.
7. **Example curl commands** for pairing, upload, SSE subscription, and local sync testing.

---

## Immediate Next Steps

1. **Scaffold core named pipe JSON RPC** and Rust client IPC module so clipboard events can be produced and consumed locally.
2. **Generate READMEs** for `core`, `client`, `server`, and `extension` as separate files as part of the deliverables.
3. **Implement server REST `POST /api/v1/clips` and Redis publish** to validate the upload path.
4. **Iterate on WebSocket/SSE delivery** and overlay UI after basic sync path is validated.
5. **Ensure code quality**: follow SOLID, write tests, and document public interfaces.

---

## Example Payloads and Commands

**Upload clip curl example**
```bash
curl -X POST "https://sync.example.com/api/v1/clips" \
  -H "Authorization: Bearer <JWT>" \
  -H "Content-Type: application/json" \
  -d '{
    "device_id":"d-123",
    "clip_id":"c-456",
    "timestamp":"2026-05-29T23:16:00Z",
    "format":"text",
    "content":"Hello world",
    "e2e": false
  }'
```

**SSE subscribe example**
```js
const es = new EventSource("https://sync.example.com/api/v1/sse?token=<JWT>");
es.addEventListener("clip", e => {
  const data = JSON.parse(e.data);
  console.log("Received clip", data);
});
```

**Local mini server push example**
```bash
curl -X POST "http://192.168.1.42:41234/local-sync" \
  -H "X-Device-Token: <DEVICE_TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{
    "device_id":"d-123",
    "clip_id":"c-789",
    "timestamp":"2026-05-29T23:20:00Z",
    "format":"text",
    "content":"Local LAN clip",
    "e2e": false
  }'
```

---

## Notes and Constraints

- **Phase 1 is text only**. Images, files, and binary clipboard formats are ignored.
- **Both desktop app and browser extension must support pairing** and can act as peers for local sync.
- **No CI/CD required for now**. Focus on clean code, tests, and documentation.
- **All code must be modular and testable** with clear trait interfaces and dependency injection where appropriate.

---

## Final Instruction for Implementation Agent

- Scaffold the projects and implement Milestone 1 first.
- Produce the required READMEs for each component as separate markdown files.
- Include the OpenAPI skeleton and SQLite migration SQL in the `docs` and `infra` folders.
- Ensure all modules are documented and include unit and integration tests.
- Follow the technical and functional requirements in this specification exactly.

---

**End of specification**