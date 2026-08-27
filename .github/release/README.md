# Protected release publication

`generate_manifest.py` is the single manifest construction path. It accepts one or more `TARGET=PATH` build artifacts, reads bytes once, records exact byte size and lowercase SHA-256, binds version/tag/full commit/targets, and signs the canonical unsigned manifest context with Ed25519. The private key is read only from `RELEASE_SIGNING_KEY`; it is never a file in this repository. Missing keys, key IDs, artifacts, invalid versions, non-HTTPS URLs, and unsigned output fail closed.

The workflow expects a preceding build to upload an artifact bundle named `release-artifacts`. The signing job exposes the protected `TAPID_RELEASE_ED25519_PRIVATE_KEY` secret only to the signing step; `TAPID_RELEASE_ED25519_PUBLIC_KEY` and `TAPID_RELEASE_SIGNING_KEY_ID` are non-secret repository/environment variables used by the real verifier. Artifact names are validated and mapped one-to-one to targets before manifest generation. The signing job and the approval-gated publication job both cryptographically verify the manifest. The `advance-stable` job uses the required GitHub protected environment `stable-release`; configure required reviewers on that environment. It refuses empty or unsigned manifests and emits deterministic `stable.json` with ordered HTTPS manifest endpoints. No preview channel or source fallback is introduced.

`stable_channel.py` intentionally publishes only provider-neutral discovery metadata. It does not sign or claim trust for a manifest; the signed manifest is separately required and uploaded by the protected workflow.

## Release sequence limitation

This foundation does not add `release_sequence`. The accepted schema (`schemas/tapid-release-manifest-v1.json`) and current release client do not expose that field, and schema/client changes are owned by other agents. The workflow therefore binds the immutable commit, version, tag, and artifact metadata that the existing contract supports. `release_sequence` must be added by its owner before replay protection can depend on it; this workflow must not be treated as implementing sequence-based rollback prevention.

## Tests

Run without repository or fixture keys:

```sh
python3 .github/release/test_release_tools.py -v
```

Tests generate an ephemeral Ed25519 key in memory and use temporary artifact files.
