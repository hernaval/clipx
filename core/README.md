# ClipX Core - Clipboard Bridge

Windows clipboard bridge that detects copy events and exposes read/write operations via JSON RPC over a named pipe.

## Responsibilities

- Detect clipboard copy events using `AddClipboardFormatListener`
- Emit events for text formats only
- Provide `get_clipboard` and `set_clipboard` operations
- Restrict pipe access to the current Windows user

## Build and Run

Dev (requires .NET 9 SDK):

```powershell
dotnet build core/ClipX.Core.csproj
dotnet run --project core/ClipX.Core.csproj
```

The bridge listens on `\\.\pipe\clipboardsync`.

## Distribute to another machine

Publish as a self-contained single-file Windows executable so the target machine doesn't need the .NET runtime:

```powershell
dotnet publish core/ClipX.Core.csproj `
  -c Release -r win-x64 --self-contained true `
  -p:PublishSingleFile=true `
  -p:IncludeNativeLibrariesForSelfExtract=true `
  -o dist/core
```

Output: `dist/core/ClipX.Core.exe` (~70 MB; bundles the .NET runtime). Copy this single file to the target machine and run it before launching `clipx-client.exe` — the Rust client will reconnect to the pipe automatically and the **Bridge** dot in the status bar will turn green.

## Configuration

No configuration files required. The bridge runs with sensible defaults.

## JSON RPC Protocol

Transport: named pipe `\\.\pipe\clipboardsync`, newline-delimited JSON.

### Notifications (server → client)

```json
{
  "jsonrpc": "2.0",
  "method": "clipboard_event",
  "params": {
    "type": "clipboard_event",
    "content": "copied text",
    "format": "text",
    "timestamp": "2026-05-29T23:16:00Z"
  }
}
```

### Requests (client → server)

**Get clipboard:**
```json
{"jsonrpc":"2.0","method":"get_clipboard","id":1}
```

**Set clipboard:**
```json
{"jsonrpc":"2.0","method":"set_clipboard","id":2,"params":{"content":"new text"}}
```

**Ping:**
```json
{"jsonrpc":"2.0","method":"ping","id":3}
```

### Response format
```json
{"jsonrpc":"2.0","id":1,"result":"clipboard content","error":null}
```

## Security

- Named pipe ACL restricted to the current Windows user
- No clipboard content is logged; only metadata is emitted to console
- Text-only filtering prevents accidental exposure of binary/image clipboard data

## Testing

```bash
dotnet build
# Run the bridge, then connect a pipe client to verify events
```
