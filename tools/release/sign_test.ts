import { strictEqual, rejects, match } from "node:assert/strict";
import { execFile } from "node:child_process";
import { generateKeyPairSync, createHash, createPrivateKey, sign as cryptoSign } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const execFileAsync = promisify(execFile);
const signer = fileURLToPath(new URL("./sign.ts", import.meta.url));

async function fixture(run: (directory: string, pem: string, seed: string) => Promise<void>) {
  const directory = await mkdtemp(join(tmpdir(), "tapid-sign-test-"));
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const pem = privateKey.export({ format: "pem", type: "pkcs8" }).toString();
  const seed = privateKey.export({ format: "der", type: "pkcs8" }).subarray(-32).toString("base64");
  try {
    await writeFile(join(directory, "record.tsv"), "tapid-release-v1\t0.0.11\n");
    await writeFile(join(directory, "keyring.json"), JSON.stringify({ keys: [{
      key_id: "fixture-key", algorithm: "ed25519",
      public_key: publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64"),
    }] }));
    await run(directory, pem, seed);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

async function sign(directory: string, key: string, keyId = "fixture-key") {
  return execFileAsync(process.execPath, ["--experimental-strip-types", signer, "sign",
    join(directory, "record.tsv"), join(directory, "record.tsv.sig"), join(directory, "keyring.json")], {
    env: { PATH: process.env.PATH, TAPID_RELEASE_SIGNING_KEY: key, TAPID_RELEASE_SIGNING_KEY_ID: keyId },
  });
}

for (const format of ["pem", "seed"]) {
  test(`signer accepts a matching ${format} key and produces a verifiable sidecar`, async () => {
    await fixture(async (directory, pem, seed) => {
      const result = await sign(directory, format === "pem" ? pem : seed);
      strictEqual(result.stdout, "");
      strictEqual(result.stderr, "");
      await execFileAsync(process.execPath, ["--experimental-strip-types", signer, "verify",
        join(directory, "record.tsv"), join(directory, "record.tsv.sig"), join(directory, "keyring.json")]);
      const envelope = JSON.parse(await readFile(join(directory, "record.tsv.sig"), "utf8"));
      strictEqual(envelope.signature.key_id, "fixture-key");
      strictEqual(envelope.claims.schema, "tapid-release-v1-immutable-signature");
      strictEqual(Object.hasOwn(envelope.claims, "expires_at"), false);
    });
  });
}

test("signer rejects a different private key before creating a sidecar", async () => {
  await fixture(async (directory) => {
    const wrong = generateKeyPairSync("ed25519").privateKey.export({ format: "pem", type: "pkcs8" }).toString();
    await rejects(() => sign(directory, wrong), error => {
      match((error as { stderr: string }).stderr, /does not match the trusted release key/);
      strictEqual((error as { stderr: string }).stderr.includes(wrong), false);
      return true;
    });
    await rejects(() => access(join(directory, "record.tsv.sig")));
  });
});

test("signer rejects unknown key IDs without writing a sidecar", async () => {
  await fixture(async (directory, pem) => {
    await rejects(() => sign(directory, pem, "unknown-key"), /key ID is not in the trusted keyring/);
    await rejects(() => access(join(directory, "record.tsv.sig")));
  });
});

for (const invalid of ["not-base64!", "AAAA", "-----BEGIN PRIVATE KEY-----\ninvalid\n-----END PRIVATE KEY-----"]) {
  test("signer rejects malformed key material without echoing it", async () => {
    await fixture(async (directory) => {
      await rejects(() => sign(directory, invalid), error => {
        const stderr = (error as { stderr: string }).stderr;
        match(stderr, /release signing key/);
        strictEqual(stderr.includes(invalid), false);
        return true;
      });
      await rejects(() => access(join(directory, "record.tsv.sig")));
    });
  });
}

test("signer rejects non-Ed25519 private keys", async () => {
  await fixture(async (directory) => {
    const key = generateKeyPairSync("ec", { namedCurve: "prime256v1" }).privateKey
      .export({ format: "pem", type: "pkcs8" }).toString();
    await rejects(() => sign(directory, key), /release signing key must be Ed25519/);
    await rejects(() => access(join(directory, "record.tsv.sig")));
  });
});

test("release signing uses the existing protected PEM secret and checks signatures before draft creation", async () => {
  const workflow = await readFile(new URL("../../.github/workflows/release-publication.yml", import.meta.url), "utf8");
  const draftJob = workflow.slice(workflow.indexOf("  draft-release:"));
  match(draftJob, /environment: stable-release/);
  match(draftJob, /TAPID_RELEASE_SIGNING_KEY: \$\{\{ secrets\.TAPID_RELEASE_ED25519_PRIVATE_KEY \}\}/);
  match(draftJob, /sign\.ts verify release\/tapid-release-v1\.tsv release\/tapid-release-v1\.tsv\.sig crates\/tapid-signatures\/data\/release-keyring\.json/);
  strictEqual(draftJob.indexOf("sign.ts verify") < draftJob.indexOf("- name: Create draft GitHub release"), true);
});

test("key check verifies identity without creating signed metadata", async () => {
  await fixture(async (directory, pem) => {
    const result = await execFileAsync(process.execPath, ["--experimental-strip-types", signer, "check-key",
      join(directory, "keyring.json")], {
      env: { PATH: process.env.PATH, TAPID_RELEASE_SIGNING_KEY: pem, TAPID_RELEASE_SIGNING_KEY_ID: "fixture-key" },
    });
    strictEqual(result.stdout, "Release signing key matches trusted keyring.\n");
    strictEqual(result.stderr, "");
    await rejects(() => access(join(directory, "record.tsv.sig")));
  });
});

test("key-check workflow runs only trusted main code with protected approval and no publication permission", async () => {
  const workflow = await readFile(new URL("../../.github/workflows/release-signing-check.yml", import.meta.url), "utf8");
  match(workflow, /workflow_dispatch:/);
  match(workflow, /if: github.ref == 'refs\/heads\/main'/);
  match(workflow, /environment: stable-release/);
  match(workflow, /ref: \$\{\{ github.sha \}\}/);
  match(workflow, /contents: read/);
  match(workflow, /sign\.ts check-key/);
  strictEqual(/contents: write|upload-artifact|cargo publish|gh release|gh api/.test(workflow), false);
});


function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b))
    .map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`).join(",")}}`;
  return JSON.stringify(value);
}

for (const scenario of ["old", "future", "expiry", "time-limited", "tampered"]) {
  test(`release tooling enforces the immutable signature contract: ${scenario}`, async () => {
    await fixture(async (directory, pem) => {
      const recordPath = join(directory, "record.tsv");
      const sidecarPath = join(directory, "record.tsv.sig");
      const keyId = "fixture-key";
      const claims: Record<string, string> = {
        schema: scenario === "time-limited" ? "tapid-release-v1-signature" : "tapid-release-v1-immutable-signature",
        created_at: scenario === "future" ? "2999-01-01T00:00:00Z" : "2000-01-01T00:00:00Z",
      };
      if (scenario === "expiry" || scenario === "time-limited") claims.expires_at = "2000-01-02T00:00:00Z";
      const digest = "sha256-" + createHash("sha256").update(await readFile(recordPath)).digest("hex");
      const payload = { artifact_digest: digest, claims, subject: "tapid-release-v1", version: "tapid-trust-envelope-v1",
        signature_context: { algorithm: "ed25519", key_id: keyId } };
      await writeFile(sidecarPath, JSON.stringify({ artifact_digest: digest, claims,
        subject: payload.subject, version: payload.version, signature: { algorithm: "ed25519",
          key_id: keyId, subject: payload.subject, artifact_digest: digest,
          value: cryptoSign(null, Buffer.from(canonical(payload)), createPrivateKey(pem)).toString("base64") } }));
      if (scenario === "tampered") await writeFile(recordPath, "changed record bytes");
      const checks = [
        () => execFileAsync(process.execPath, ["--experimental-strip-types", signer, "verify", recordPath, sidecarPath,
          join(directory, "keyring.json")]),
      ];
      for (const check of checks) {
        if (scenario === "old") await check();
        else await rejects(check);
      }
    });
  });
}
