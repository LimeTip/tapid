import { releaseVersion } from "./release.ts";

/** Compares canonical stable semver versions; prereleases and invalid forms are rejected. */
export function compareStableVersions(left: string, right: string): number {
  const parse = (version: string): bigint[] => {
    if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(version)) {
      throw new Error(`invalid stable version: ${version}`);
    }
    releaseVersion(`v${version}`, version);
    return version.split(".").map(BigInt);
  };
  const a = parse(left);
  const b = parse(right);
  for (let index = 0; index < 3; index++) {
    if (a[index] !== b[index]) return a[index] > b[index] ? 1 : -1;
  }
  return 0;
}
