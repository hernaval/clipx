# ClipX Sequence Diagrams

## Copy Flow

```mermaid
sequenceDiagram
    participant App as Application
    participant Core as C# Core Bridge
    participant Client as Rust Client
    participant DB as Local SQLite
    participant Server as ClipX Server
    participant Redis as Redis Pub/Sub

    App->>Core: User copies text (Ctrl+C)
    Core->>Core: AddClipboardFormatListener detects change
    Core->>Core: Read clipboard (CF_UNICODETEXT)
    Core->>Client: JSON RPC notification via named pipe
    Note over Core,Client: {"jsonrpc":"2.0","method":"clipboard_event","params":{...}}
    Client->>DB: Store clip (status: pending)
    Client->>Server: POST /api/v1/clips (JWT auth)
    Server->>Server: Validate JWT, check idempotency
    Server->>Server: Store clip in DB
    Server->>Redis: PUBLISH clips:user:<user_id>
    Server-->>Client: 201 Created
    Client->>DB: Remove from queue
```

## Sync Flow (Real-Time Delivery)

```mermaid
sequenceDiagram
    participant ClientA as Client A (sender)
    participant Server as ClipX Server
    participant Redis as Redis Pub/Sub
    participant ClientB as Client B (WebSocket)
    participant Extension as Browser Extension (SSE)

    ClientA->>Server: POST /api/v1/clips
    Server->>Redis: PUBLISH clips:user:<user_id>
    Redis->>Server: Message on subscription
    
    par WebSocket delivery
        Server->>ClientB: WebSocket frame {"type":"clip",...}
        ClientB->>ClientB: Store clip locally
        ClientB->>ClientB: Update overlay UI
    and SSE delivery
        Server->>Extension: SSE event: clip
        Extension->>Extension: Store in IndexedDB
        Extension->>Extension: Show badge notification
    end
```

## Paste Flow (Overlay)

```mermaid
sequenceDiagram
    participant User
    participant Client as Rust Client
    participant DB as Local SQLite
    participant System as System Clipboard

    User->>Client: Press hotkey (Ctrl+Shift+V)
    Client->>DB: Fetch recent clips
    Client->>Client: Show overlay UI
    User->>Client: Navigate (Up/Down) + Enter
    Client->>System: Set clipboard text (arboard)
    Client->>Client: Hide overlay
    Client->>System: Simulate Ctrl+V (SendInput)
    System->>User: Text pasted in active app
```

## Device Pairing Flow

```mermaid
sequenceDiagram
    participant DevA as Device A (existing)
    participant Server as ClipX Server
    participant DevB as Device B (new)

    DevA->>Server: POST /api/v1/pair/request (JWT auth)
    Server->>Server: Generate pairing token (10min expiry)
    Server-->>DevA: {token, expires_at}
    DevA->>DevB: Share token (QR code or manual)
    DevB->>Server: POST /api/v1/pair/confirm {token, device_name, public_key}
    Server->>Server: Validate token, create device
    Server->>Server: Generate JWT for new device
    Server->>Server: Log audit: device_paired
    Server-->>DevB: {device_id, device_token, jwt}
    DevB->>Server: Connect WebSocket with JWT
```

## LAN Pair Flow (server-independent)

```mermaid
sequenceDiagram
    participant UserA as User on A
    participant DevA as Device A (clipx-client)
    participant DevB as Device B (clipx-client)
    participant UserB as User on B

    UserA->>DevA: Enter B's IP in "Pair by IP" → Request Pair
    DevA->>DevB: POST /pair/request {device_id, pubkey, ip, port, session_token}
    DevB-->>DevA: 202 Accepted
    DevB->>UserB: Show modal "Device A wants to sync"
    UserB->>DevB: Accept
    DevB->>DevB: Insert A into peer_devices (with session_token)
    DevB->>DevA: POST /pair/accept {device_id, pubkey, ip, port, session_token}
    DevA->>DevA: Insert B into peer_devices (same session_token)
    DevA-->>DevB: 200 OK

    Note over DevA,DevB: LanPeerManager opens outbound WebSocket
    DevA->>DevB: GET ws://B/ws?token=<session_token>
    DevB->>DevB: Validate token against peer_devices → upgrade
    DevB-->>DevA: WebSocket established (clips flow both ways)
```

## LAN Clip Sync (after pairing)

```mermaid
sequenceDiagram
    participant Core as C# Core Bridge (A)
    participant ClientA as Rust Client (A)
    participant ClientB as Rust Client (B)
    participant CoreB as C# Core Bridge (B)
    participant SysB as System Clipboard (B)

    Core->>ClientA: clipboard_event {content}
    ClientA->>ClientA: Store clip in local SQLite
    ClientA->>ClientB: WS frame {"type":"clip", payload:{...}}
    ClientB->>ClientB: Insert clip in local SQLite (dedup by id)
    ClientB->>ClientB: Push to UI history
    opt auto_paste_inbound = true
        ClientB->>CoreB: JSON-RPC set_clipboard {content}
        CoreB->>SysB: Write OS clipboard
    end
```

## Cloud Offline Queue Flow

```mermaid
sequenceDiagram
    participant Client as Rust Client
    participant Queue as Local Queue (SQLite)
    participant Server as ClipX Server

    Client->>Server: POST /api/v1/clips
    Server-->>Client: Connection refused / timeout
    Client->>Queue: Enqueue clip (status: pending)

    loop Exponential backoff
        Queue->>Client: Next pending clip whose next_attempt_at <= now
        Client->>Server: POST /api/v1/clips (retry)
        alt Server reachable
            Server-->>Client: 201 Created
            Client->>Queue: Remove entry
        else Still offline
            Client->>Queue: Increment attempts, schedule retry
            Note over Queue: backoff: 2^attempts seconds (max 5min)
            Note over Client: After max_cloud_retries the listener pauses 5min, then resumes
        end
    end
```

Note: LAN sync (above) does not use the offline queue — it broadcasts directly to any connected LAN peer WebSocket regardless of cloud state.

## Device Revocation Flow

```mermaid
sequenceDiagram
    participant User
    participant Client as Authorized Client
    participant Server as ClipX Server
    participant Revoked as Revoked Device

    User->>Client: Revoke device via UI
    Client->>Server: DELETE /api/v1/devices/{id} (JWT auth)
    Server->>Server: Mark device revoked
    Server->>Server: Log audit: device_revoked
    Server-->>Client: 204 No Content
    
    Revoked->>Server: Any API call with JWT
    Server->>Server: Check device.revoked = true
    Server-->>Revoked: 403 Forbidden (device_revoked)
    
    Revoked->>Server: WebSocket connect
    Server-->>Revoked: 403 Forbidden
```
