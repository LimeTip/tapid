import { createHash, createPrivateKey, createPublicKey, sign, verify } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";

const PKCS8_ED25519_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const SPKI_ED25519_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const SIGNATURE_SCHEMA = "tapid-release-v1-signature";
const SUBJECT = "tapid-release-v1";

function sortJson(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortJson);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, item]) => [key, sortJson(item)]));
  }
  return value;
}

function canonicalJson(value: unknown): Buffer {
  return Buffer.from(JSON.stringify(sortJson(value)));
}

function keyFromSeed(seed: Buffer) {
  if (seed.length !== 32) throw new Error("release signing key must decode to 32 bytes");
  return createPrivateKey({ key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]), format: "der", type: "pkcs8" });
}

function publicKeyFromBytes(publicKey: Buffer) {
  if (publicKey.length !== 32) throw new Error("release public key must be 32 bytes");
  return createPublicKey({ key: Buffer.concat([SPKI_ED25519_PREFIX, publicKey]), format: "der", type: "spki" });
}

function createHashHex(bytes: Buffer): string {
  return createHash("sha256").update(bytes).digest("hex");
}

function signingPayload(record: Buffer, keyId: string, createdAt: string, expiresAt: string) {
  return {
    artifact_digest: `sha256-${createHashHex(record)}`,
    claims: { created_at: createdAt, expires_at: expiresAt, schema: SIGNATURE_SCHEMA },
    subject: SUBJECT,
    version: "tapid-trust-envelope-v1",
    signature_context: { algorithm: "ed25519", key_id: keyId },
  };
}

async function signRecord(recordPath: string, sidecarPath: string): Promise<void> {
  const encodedSeed = process.env.TAPID_RELEASE_SIGNING_KEY;
  if (!encodedSeed) throw new Error("TAPID_RELEASE_SIGNING_KEY is required");
  const seed = Buffer.from(encodedSeed, "base64");
  const keyId = process.env.TAPID_RELEASE_SIGNING_KEY_ID;
  if (!keyId) throw new Error("TAPID_RELEASE_SIGNING_KEY_ID is required");
  const record = await readFile(recordPath);
  const createdAt = new Date().toISOString().replace(".000Z", "Z");
  const expiresAt = new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toISOString().replace(".000Z", "Z");
  const payload = signingPayload(record, keyId, createdAt, expiresAt);
  const signature = sign(null, canonicalJson(payload), keyFromSeed(seed));
  await writeFile(sidecarPath, JSON.stringify({
    artifact_digest: payload.artifact_digest,
    claims: payload.claims,
    signature: {
      algorithm: "ed25519",
      artifact_digest: payload.artifact_digest,
      key_id: keyId,
      subject: SUBJECT,
      value: signature.toString("base64"),
    },
    subject: SUBJECT,
    version: "tapid-trust-envelope-v1",
  }) + "\n");
}

async function verifyRecord(recordPath: string, sidecarPath: string, keyringPath: string): Promise<void> {
  const record = await readFile(recordPath);
  const envelope = JSON.parse(await readFile(sidecarPath, "utf8"));
  const keyring = JSON.parse(await readFile(keyringPath, "utf8"));
  const key = keyring.keys.find((candidate: { key_id: string }) => candidate.key_id === envelope.signature?.key_id);
  if (!key) throw new Error("sidecar key is not in the trusted keyring");
  const publicKey = publicKeyFromBytes(Buffer.from(key.public_key, "base64"));
  const payload = {
    artifact_digest: envelope.artifact_digest,
    claims: envelope.claims,
    subject: envelope.subject,
    version: envelope.version,
    signature_context: { algorithm: envelope.signature.algorithm, key_id: envelope.signature.key_id },
  };
  if (envelope.artifact_digest !== `sha256-${createHashHex(record)}` || envelope.subject !== SUBJECT) {
    throw new Error("sidecar does not bind the release record");
  }
  if (!verify(null, canonicalJson(payload), publicKey, Buffer.from(envelope.signature.value, "base64"))) {
    throw new Error("release record signature verification failed");
  }
}

const [command, record, sidecar, keyring] = process.argv.slice(2);
if (command === "sign" && record && sidecar) {
  await signRecord(record, sidecar);
} else if (command === "verify" && record && sidecar && keyring) {
  await verifyRecord(record, sidecar, keyring);
} else {
  console.error("usage: sign.ts sign RECORD SIDECAR | verify RECORD SIDECAR KEYRING");
  process.exitCode = 2;
}
