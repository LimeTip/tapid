const requiredEvidence = [
  "Release identity",
  "Release-preparation PR",
  "Changed supporting crates and reason",
  "Package/version matrix",
  "Immutable source references",
  "Annotated tag",
  "Peeled commit",
  "Binary workflow run ID",
  "Recovery run ID",
  "GitHub release and asset verification",
  "Public release ID",
  "Asset read-back",
  "SHA-256 read-back",
  "Public smoke evidence",
  "Public smoke run ID",
  "Unix installer (ubuntu-latest)",
  "Unix installer (macos-latest)",
  "Windows installer",
  "crates.io evidence and approval boundary",
  "Dry-run plan",
  "Separate crates.io approval",
  "crates.io workflow run ID",
  "Registry read-back",
  "Clean command",
  "Clean install output",
  "Limitations and follow-ups",
  "Final repository, release, tag, workflow, and registry state read-back",
  "GitHub publication approval was separate from crates.io approval.",
];

const forbiddenEvidence = [
  /gh\s+(workflow\s+(run|dispatch)|release\s+(create|publish|edit))/i,
  /\bcargo\s+publish\b/i,
  /workflow_dispatch:/i,
  /(?:^|\s)(?:password|passphrase|secret|token|cookie|private key|signing key)\s*[:=]\s*[^\s`[]+/i,
];

export function validateReleaseEvidence(markdown: string): void {
  const missing = requiredEvidence.filter((entry) => !markdown.includes(entry));
  if (missing.length > 0) throw new Error(`release evidence is missing: ${missing.join(", ")}`);

  const forbidden = forbiddenEvidence.filter((pattern) => pattern.test(markdown));
  if (forbidden.length > 0) throw new Error("release evidence contains a publication trigger or secret value");
}
