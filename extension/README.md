# ClipX Browser Extension

Manifest V3 browser extension for Chrome/Edge that provides manual clipboard push, SSE subscription for incoming clips, popup history UI, and device pairing.

## Responsibilities

- Manual push of clipboard text to the ClipX server
- Subscribe to real-time clip updates via SSE (Server-Sent Events)
- Display popup UI with clip history and copy-to-clipboard
- Pair with the ClipX server using a pairing token
- Queue clips in IndexedDB when offline, retry automatically

## Installation

1. Open Chrome/Edge and navigate to `chrome://extensions`
2. Enable "Developer mode"
3. Click "Load unpacked" and select the `extension/` directory

## Usage

### Pairing

1. Click the ClipX extension icon
2. Click "Pair"
3. Enter your server URL (e.g., `http://localhost:8080`)
4. Enter the pairing token from your desktop app or server
5. Click "Confirm Pairing"

### Push Clipboard

1. Copy text in any application
2. Click the ClipX extension icon
3. Click "Push Clipboard"

The clip will be uploaded to the server and synced to all paired devices.

### View History

The popup shows the 20 most recent clips. Click any clip to copy it to your clipboard.

## Architecture

| File | Purpose |
|------|---------|
| `manifest.json` | Extension manifest (MV3) |
| `popup.html` | Popup UI layout and styles |
| `src/popup.js` | Popup logic: push, pair, history, offline queue |
| `src/background.js` | Service worker: SSE subscription, queue processing |

## Offline Support

When the server is unreachable:
- Clips are stored in IndexedDB
- Background service worker retries every 30 seconds
- Max 10 retry attempts per clip
- Clips are synced automatically when connection is restored

## Security

- All API calls use HTTPS with JWT authentication
- Pairing tokens are single-use and expire after 10 minutes
- Clipboard access requires explicit user interaction (Manifest V3)
- No content is logged to console in production

## Permissions

- `storage` — persist config and clip cache
- `clipboardRead` — read clipboard for push
- `clipboardWrite` — write clipboard on copy
- Host permissions — API access to configured server
