/** Stable numeric versions only. Publication metadata may contain unsupported versions. */
export function stableVersionParts(version: string): bigint[] | undefined {
  const match = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.exec(version);
  return match ? match.slice(1).map(BigInt) : undefined;
}

export function compareStableVersions(left: string, right: string): number | undefined {
  const a = stableVersionParts(left), b = stableVersionParts(right);
  if (!a || !b) return undefined;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  }
  return 0;
}
