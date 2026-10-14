import { strict as assert } from "node:assert";
import { createPublicKey, verify } from "node:crypto";
import { join } from "node:path";
import { test } from "node:test";
import { signRecord } from "../scripts/check-release-record.ts";
import { temporary, write, json, sha256 } from "./script-support.ts";

test("offline release signer binds exact record and signature context", async () =>
  temporary((path) => {
    const record = join(path, "record.tsv"),
      signature = join(path, "record.sig"),
      keyring = join(path, "keyring.json");
    write(record, "tapid-release-v1\t1.2.3\n");
    signRecord(record, signature, keyring);
    const envelope = json(signature),
      key = json(keyring).keys[0],
      publicBytes = Buffer.from(key.public_key, "base64");
    assert.equal(key.fingerprint, "sha256-" + sha256(publicBytes));
    assert.equal(
      envelope.artifact_digest,
      "sha256-" + sha256("tapid-release-v1\t1.2.3\n"),
    );
    const payload = JSON.stringify({
      artifact_digest: envelope.artifact_digest,
      claims: {
        created_at: envelope.claims.created_at,
        schema: envelope.claims.schema,
      },
      signature_context: { algorithm: "ed25519", key_id: key.key_id },
      subject: envelope.subject,
      version: envelope.version,
    });
    const publicKey = createPublicKey({
      key: Buffer.concat([
        Buffer.from("302a300506032b6570032100", "hex"),
        publicBytes,
      ]),
      format: "der",
      type: "spki",
    });
    assert.ok(
      verify(
        null,
        Buffer.from(payload),
        publicKey,
        Buffer.from(envelope.signature.value, "base64"),
      ),
    );
    assert.ok(
      !verify(
        null,
        Buffer.from(payload.replace("tapid-release-v1", "tapid-release-v99")),
        publicKey,
        Buffer.from(envelope.signature.value, "base64"),
      ),
    );
  }));
