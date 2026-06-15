// E2E decryption for ClipX clips.
//
// Wire-compatible with `client/src/crypto.rs` which uses `crypto_box::ChaChaBox`,
// i.e. NaCl's crypto_box_curve25519xchacha20poly1305 construction (X25519 + HSalsa20
// KDF + XChaCha20Poly1305 AEAD, 24-byte nonce). The matching libsodium function is
// `crypto_box_curve25519xchacha20poly1305_easy` / `_open_easy`.
//
// Pack format mirrors `crypto.rs::pack`:
//   "e2e:1:" + base64(JSON({ v, recipients: [{device_id, ephemeral_pub, nonce, ciphertext}] }))
// All inner fields are base64-encoded (standard alphabet, with padding).

import sodium from '../vendor/libsodium-wrappers.mjs';

let ready;
function ensureReady() {
  if (!ready) ready = sodium.ready;
  return ready;
}

function b64Decode(s) {
  // STANDARD base64 with padding — matches Rust's base64::STANDARD.
  return sodium.from_base64(s, sodium.base64_variants.ORIGINAL);
}

function b64Encode(bytes) {
  return sodium.to_base64(bytes, sodium.base64_variants.ORIGINAL);
}

export async function generateKeyPair() {
  await ensureReady();
  // Curve25519 keys are identical across NaCl box variants — the XChaCha-named
  // keypair function isn't exposed by libsodium-wrappers, so we use the base
  // crypto_box_keypair. The resulting 32-byte keys feed _easy / _open_easy below
  // verbatim and match Rust's `crypto_box::SecretKey::generate`.
  const kp = sodium.crypto_box_keypair();
  return {
    publicKey: b64Encode(kp.publicKey),
    privateKey: b64Encode(kp.privateKey),
  };
}

// Parse "e2e:1:<base64>" → { v, recipients } or null.
export async function unpack(content) {
  if (typeof content !== 'string') return null;
  const PREFIX = 'e2e:1:';
  if (!content.startsWith(PREFIX)) return null;
  await ensureReady();
  try {
    const jsonBytes = b64Decode(content.slice(PREFIX.length));
    const json = new TextDecoder().decode(jsonBytes);
    const obj = JSON.parse(json);
    if (!obj || obj.v !== 1 || !Array.isArray(obj.recipients)) return null;
    return obj;
  } catch {
    return null;
  }
}

// Returns plaintext string or throws.
export async function decryptForDevice(encrypted, myDeviceId, myPrivateB64) {
  await ensureReady();
  const entry = encrypted.recipients.find((r) => r.device_id === myDeviceId);
  if (!entry) throw new Error('clip not addressed to this device');
  const senderPub = b64Decode(entry.ephemeral_pub);
  const secret = b64Decode(myPrivateB64);
  const nonce = b64Decode(entry.nonce);
  const ct = b64Decode(entry.ciphertext);
  const pt = sodium.crypto_box_curve25519xchacha20poly1305_open_easy(
    ct,
    nonce,
    senderPub,
    secret,
  );
  return new TextDecoder().decode(pt);
}

// Mutates clip.content in place if it's an E2E envelope we can decrypt.
// Returns:
//   "plaintext" — was already plain, nothing changed
//   "decrypted" — was encrypted, content replaced with plaintext
//   "skip"      — was encrypted but not for us / corrupt
export async function tryDecryptInPlace(clip, deviceId, privateKey) {
  if (!clip || typeof clip.content !== 'string') return 'plaintext';
  const enc = await unpack(clip.content);
  if (!enc) return 'plaintext';
  if (!deviceId || !privateKey) return 'skip';
  try {
    const pt = await decryptForDevice(enc, deviceId, privateKey);
    clip.content = pt;
    return 'decrypted';
  } catch {
    return 'skip';
  }
}
