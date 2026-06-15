import { generateKeyPair } from './crypto.js';

// Returns the device's existing X25519 keypair, generating + persisting one if missing.
async function ensureKeyPair() {
  const data = await chrome.storage.local.get(['publicKey', 'privateKey']);
  if (data.publicKey && data.privateKey) {
    return { publicKey: data.publicKey, privateKey: data.privateKey };
  }
  const kp = await generateKeyPair();
  await chrome.storage.local.set({ publicKey: kp.publicKey, privateKey: kp.privateKey });
  return kp;
}

const statusEl = document.getElementById('status');
const pushBtn = document.getElementById('push-btn');
const pairBtn = document.getElementById('pair-btn');
const backBtn = document.getElementById('back-btn');
const mainView = document.getElementById('main-view');
const pairHint = document.getElementById('pair-hint');
const pairHintBtn = document.getElementById('pair-hint-btn');
const pairingSection = document.getElementById('pairing-section');
const confirmPairBtn = document.getElementById('confirm-pair');
const clipList = document.getElementById('clip-list');
const toast = document.getElementById('toast');
const serverUrlInput = document.getElementById('server-url');
const pairTokenInput = document.getElementById('pair-token');
const pairUriInput = document.getElementById('pair-uri');
const applyUriBtn = document.getElementById('apply-uri');

// Parse a clipx://pair?server=...&token=... URI into {serverUrl, token}.
function parsePairUri(uri) {
  if (!uri) return null;
  const t = uri.trim();
  if (!t.toLowerCase().startsWith('clipx://pair')) return null;
  const qIdx = t.indexOf('?');
  if (qIdx < 0) return null;
  const params = new URLSearchParams(t.slice(qIdx + 1));
  const server = params.get('server');
  const token = params.get('token');
  if (!server || !token) return null;
  return { serverUrl: server, token };
}

// Apply the parsed URI into the form fields; user still confirms.
applyUriBtn?.addEventListener('click', () => {
  const parsed = parsePairUri(pairUriInput.value);
  if (!parsed) {
    showToast('Invalid clipx:// URI');
    return;
  }
  serverUrlInput.value = parsed.serverUrl;
  pairTokenInput.value = parsed.token;
  pairUriInput.value = '';
  showToast('URI applied — click Confirm Pairing');
});

// Auto-fill if the user pastes a URI directly into the URI field.
pairUriInput?.addEventListener('paste', (ev) => {
  const text = (ev.clipboardData || window.clipboardData)?.getData('text');
  const parsed = parsePairUri(text);
  if (parsed) {
    ev.preventDefault();
    serverUrlInput.value = parsed.serverUrl;
    pairTokenInput.value = parsed.token;
    pairUriInput.value = '';
    showToast('URI applied — click Confirm Pairing');
  }
});

let config = { serverUrl: '', jwt: '', deviceId: '' };

async function loadConfig() {
  const data = await chrome.storage.local.get(['serverUrl', 'jwt', 'deviceId']);
  config.serverUrl = data.serverUrl || '';
  config.jwt = data.jwt || '';
  config.deviceId = data.deviceId || '';
  serverUrlInput.value = config.serverUrl;
  updateStatus();
}

function updateStatus() {
  if (config.jwt) {
    statusEl.textContent = 'Connected';
    statusEl.className = 'status connected';
    pairHint.classList.remove('show');
  } else {
    statusEl.textContent = 'Disconnected';
    statusEl.className = 'status disconnected';
    pairHint.classList.add('show');
  }
}

function openPairingSection() {
  pairingSection.classList.add('active');
  mainView.classList.add('hidden');
  pairBtn.classList.add('active');
}

function closePairingSection() {
  pairingSection.classList.remove('active');
  mainView.classList.remove('hidden');
  pairBtn.classList.remove('active');
}

function showToast(msg) {
  toast.textContent = msg;
  toast.classList.add('show');
  setTimeout(() => toast.classList.remove('show'), 2000);
}

// Push clipboard
pushBtn.addEventListener('click', async () => {
  if (!config.jwt || !config.serverUrl) {
    showToast('Please pair first');
    openPairingSection();
    return;
  }

  try {
    const text = await navigator.clipboard.readText();
    if (!text) {
      showToast('Clipboard is empty');
      return;
    }

    const clipId = crypto.randomUUID();
    const resp = await fetch(`${config.serverUrl}/api/v1/clips`, {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${config.jwt}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        device_id: config.deviceId,
        clip_id: clipId,
        timestamp: new Date().toISOString(),
        format: 'text',
        content: text,
        e2e: false,
      }),
    });

    if (resp.ok || resp.status === 409) {
      showToast('Pushed!');
      addClipToQueue(clipId, text);
      await loadClips();
    } else if (resp.status === 401) {
      showToast('Auth expired - re-pair');
    } else {
      showToast('Push failed');
    }
  } catch (err) {
    // Offline - queue it
    const text = await navigator.clipboard.readText().catch(() => null);
    if (text) {
      await queueClip(text);
      showToast('Queued for sync');
    } else {
      showToast('Cannot read clipboard');
    }
  }
});

// Pair / settings navigation
pairBtn.addEventListener('click', () => {
  openPairingSection();
});

backBtn?.addEventListener('click', () => {
  closePairingSection();
});

pairHintBtn?.addEventListener('click', () => {
  openPairingSection();
});

// Confirm pairing
confirmPairBtn.addEventListener('click', async () => {
  const serverUrl = serverUrlInput.value.trim().replace(/\/$/, '');
  const token = pairTokenInput.value.trim();

  if (!serverUrl || !token) {
    showToast('Fill in all fields');
    return;
  }

  try {
    const kp = await ensureKeyPair();
    const resp = await fetch(`${serverUrl}/api/v1/pair/confirm`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        token: token,
        device_name: 'Browser Extension',
        public_key: kp.publicKey,
      }),
    });

    if (resp.ok) {
      const data = await resp.json();
      config.serverUrl = serverUrl;
      config.jwt = data.jwt;
      config.deviceId = data.device_id;

      await chrome.storage.local.set({
        serverUrl: config.serverUrl,
        jwt: config.jwt,
        deviceId: config.deviceId,
      });

      closePairingSection();
      updateStatus();
      showToast('Paired!');

      // Start SSE
      chrome.runtime.sendMessage({ type: 'start_sse' });
      await loadClips();
    } else {
      showToast('Pairing failed');
    }
  } catch (err) {
    showToast('Connection error');
  }
});

// Load clips from server
async function loadClips() {
  if (!config.jwt || !config.serverUrl) return;

  try {
    const resp = await fetch(`${config.serverUrl}/api/v1/clips?limit=20`, {
      headers: { 'Authorization': `Bearer ${config.jwt}` },
    });

    if (resp.ok) {
      const clips = await resp.json();
      renderClips(clips);
    }
  } catch {
    // Load from local storage
    const data = await chrome.storage.local.get(['clips']);
    if (data.clips) renderClips(data.clips);
  }
}

function renderClips(clips) {
  if (!clips.length) {
    clipList.innerHTML = '<div class="empty">No clips yet.</div>';
    return;
  }

  clipList.innerHTML = clips
    .map(
      (clip) => `
    <div class="clip-item" data-content="${escapeHtml(clip.content)}">
      <div class="clip-content">${escapeHtml(clip.content)}</div>
      <div class="clip-meta">${formatTime(clip.created_at)} · ${clip.device_id?.slice(0, 8) || 'unknown'}</div>
    </div>
  `
    )
    .join('');

  clipList.querySelectorAll('.clip-item').forEach((item) => {
    item.addEventListener('click', async () => {
      const content = item.dataset.content;
      await navigator.clipboard.writeText(content);
      showToast('Copied!');
    });
  });

  // Cache clips locally
  chrome.storage.local.set({ clips: clips.slice(0, 50) });
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

function formatTime(ts) {
  try {
    const d = new Date(ts);
    const now = new Date();
    const diff = now - d;
    if (diff < 60000) return 'just now';
    if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
    if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
    return d.toLocaleDateString();
  } catch {
    return '';
  }
}

// Offline queue using IndexedDB
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

async function queueClip(content) {
  const db = await openQueue();
  const tx = db.transaction('queue', 'readwrite');
  tx.objectStore('queue').add({
    id: crypto.randomUUID(),
    content,
    timestamp: new Date().toISOString(),
    attempts: 0,
  });
}

async function addClipToQueue(clipId, content) {
  // No-op for already-pushed clips, but store locally for history
  const data = await chrome.storage.local.get(['clips']);
  const clips = data.clips || [];
  clips.unshift({
    id: clipId,
    content,
    created_at: new Date().toISOString(),
    device_id: config.deviceId,
    format: 'text',
    e2e: false,
  });
  await chrome.storage.local.set({ clips: clips.slice(0, 50) });
}

// Init
loadConfig().then(() => loadClips());
