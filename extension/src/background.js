import { generateKeyPair, tryDecryptInPlace } from './crypto.js';
import { insertTextAtCaret } from './insertText.js';

let eventSource = null;

const PARENT_MENU_ID = 'clipx-paste-parent';
const CHILD_ID_PREFIX = 'clipx-paste-child-';
const MENU_LIMIT = 10;
const MENU_LABEL_CHARS = 50;

chrome.runtime.onMessage.addListener((msg) => {
  if (msg.type === 'start_sse') {
    startSSE();
  }
});

chrome.runtime.onStartup.addListener(async () => {
  await ensureKeyPair();
  startSSE();
  rebuildContextMenu();
});

chrome.runtime.onInstalled.addListener(async () => {
  await ensureKeyPair();
  startSSE();
  rebuildContextMenu();
});

// Rebuild the right-click menu whenever the cached clip list changes.
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === 'local' && changes.clips) {
    rebuildContextMenu();
  }
});

// Ensure an X25519 keypair exists in local storage; generate one on first run.
async function ensureKeyPair() {
  const data = await chrome.storage.local.get(['publicKey', 'privateKey']);
  if (data.publicKey && data.privateKey) return;
  const kp = await generateKeyPair();
  await chrome.storage.local.set({ publicKey: kp.publicKey, privateKey: kp.privateKey });
  console.log('[ClipX] Generated extension keypair');
}

async function startSSE() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }

  const data = await chrome.storage.local.get(['serverUrl', 'jwt']);
  if (!data.serverUrl || !data.jwt) return;

  const url = `${data.serverUrl}/api/v1/sse?token=${data.jwt}`;

  try {
    eventSource = new EventSource(url);

    eventSource.addEventListener('clip', (e) => {
      try {
        const clip = JSON.parse(e.data);
        handleIncomingClip(clip);
      } catch {
        // ignore parse errors
      }
    });

    eventSource.onerror = () => {
      eventSource.close();
      eventSource = null;
      setTimeout(startSSE, 10000);
    };

    console.log('[ClipX] SSE connected');
  } catch (err) {
    console.error('[ClipX] SSE connection failed:', err);
    setTimeout(startSSE, 10000);
  }
}

async function handleIncomingClip(clip) {
  const data = await chrome.storage.local.get([
    'clips',
    'deviceId',
    'privateKey',
  ]);

  if (clip.device_id === data.deviceId) return;

  const entry = {
    id: clip.clip_id,
    content: clip.content,
    created_at: clip.timestamp,
    device_id: clip.device_id,
    format: 'text',
    e2e: clip.e2e || false,
  };

  const result = await tryDecryptInPlace(entry, data.deviceId, data.privateKey);
  if (result === 'skip') {
    // E2E clip not addressed to us, or corrupt. Don't cache it.
    return;
  }

  const clips = data.clips || [];
  clips.unshift(entry);
  await chrome.storage.local.set({ clips: clips.slice(0, 50) });

  chrome.action.setBadgeText({ text: '!' });
  chrome.action.setBadgeBackgroundColor({ color: '#e94560' });
  setTimeout(() => chrome.action.setBadgeText({ text: '' }), 5000);
}

// ── Context menu ───────────────────────────────────────────────────────────────

function makeLabel(content) {
  const collapsed = content.replace(/\s+/g, ' ').trim();
  if (collapsed.length <= MENU_LABEL_CHARS) return collapsed;
  return collapsed.slice(0, MENU_LABEL_CHARS - 1) + '…';
}

async function rebuildContextMenu() {
  try {
    await chrome.contextMenus.removeAll();
  } catch {
    // ignore
  }

  const data = await chrome.storage.local.get(['clips']);
  const all = data.clips || [];
  // Skip any clips that are still e2e-wrapped (means we couldn't decrypt them,
  // showing them in the menu would paste ciphertext).
  const usable = all.filter(
    (c) => typeof c.content === 'string' && !c.content.startsWith('e2e:1:'),
  );
  const items = usable.slice(0, MENU_LIMIT);

  if (items.length === 0) {
    // Still show the parent so users know the menu exists, with a disabled placeholder.
    chrome.contextMenus.create({
      id: PARENT_MENU_ID,
      title: 'Paste with ClipX',
      contexts: ['editable'],
    });
    chrome.contextMenus.create({
      id: CHILD_ID_PREFIX + 'empty',
      parentId: PARENT_MENU_ID,
      title: '(no clips yet)',
      enabled: false,
      contexts: ['editable'],
    });
    return;
  }

  chrome.contextMenus.create({
    id: PARENT_MENU_ID,
    title: 'Paste with ClipX',
    contexts: ['editable'],
  });

  items.forEach((clip, idx) => {
    chrome.contextMenus.create({
      id: CHILD_ID_PREFIX + idx,
      parentId: PARENT_MENU_ID,
      title: makeLabel(clip.content),
      contexts: ['editable'],
    });
  });
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (!info.menuItemId || typeof info.menuItemId !== 'string') return;
  if (!info.menuItemId.startsWith(CHILD_ID_PREFIX)) return;
  const idxStr = info.menuItemId.slice(CHILD_ID_PREFIX.length);
  const idx = parseInt(idxStr, 10);
  if (!Number.isFinite(idx)) return;

  const data = await chrome.storage.local.get(['clips']);
  const usable = (data.clips || []).filter(
    (c) => typeof c.content === 'string' && !c.content.startsWith('e2e:1:'),
  );
  const clip = usable[idx];
  if (!clip || !tab?.id) return;

  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [info.frameId ?? 0] },
      func: insertTextAtCaret,
      args: [clip.content],
    });
  } catch (e) {
    console.warn('[ClipX] Paste failed:', e);
  }
});

// ── Outbound clip queue (unchanged below) ─────────────────────────────────────

setInterval(processQueue, 30000);

async function processQueue() {
  const data = await chrome.storage.local.get(['serverUrl', 'jwt', 'deviceId']);
  if (!data.serverUrl || !data.jwt) return;

  try {
    const db = await openQueue();
    const tx = db.transaction('queue', 'readonly');
    const store = tx.objectStore('queue');
    const items = await getAllFromStore(store);

    for (const item of items) {
      try {
        const resp = await fetch(`${data.serverUrl}/api/v1/clips`, {
          method: 'POST',
          headers: {
            'Authorization': `Bearer ${data.jwt}`,
            'Content-Type': 'application/json',
          },
          body: JSON.stringify({
            device_id: data.deviceId,
            clip_id: item.id,
            timestamp: item.timestamp,
            format: 'text',
            content: item.content,
            e2e: false,
          }),
        });

        if (resp.ok || resp.status === 409) {
          const delTx = db.transaction('queue', 'readwrite');
          delTx.objectStore('queue').delete(item.id);
        } else if (resp.status !== 401) {
          const updTx = db.transaction('queue', 'readwrite');
          item.attempts += 1;
          if (item.attempts >= 10) {
            updTx.objectStore('queue').delete(item.id);
          } else {
            updTx.objectStore('queue').put(item);
          }
        }
      } catch {
        // Network error, will retry later
      }
    }
  } catch {
    // IndexedDB error
  }
}

function openQueue() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('clipx-queue', 1);
    req.onupgradeneeded = (e) => {
      const db = e.target.result;
      if (!db.objectStoreNames.contains('queue')) {
        db.createObjectStore('queue', { keyPath: 'id' });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function getAllFromStore(store) {
  return new Promise((resolve, reject) => {
    const req = store.getAll();
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
