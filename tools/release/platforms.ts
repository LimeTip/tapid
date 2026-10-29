/** Ordered release platforms. Native installer support remains explicit and contract-tested. */
export const PLATFORMS = [
  { target: 'aarch64-apple-darwin', binary: 'tapid', runner: 'macos-14' },
  { target: 'aarch64-pc-windows-msvc', binary: 'tapid.exe', runner: 'windows-11-vs2026-arm' },
  { target: 'aarch64-unknown-linux-gnu', binary: 'tapid', runner: 'ubuntu-24.04-arm' },
  { target: 'x86_64-apple-darwin', binary: 'tapid', runner: 'macos-15-intel' },
  { target: 'x86_64-pc-windows-msvc', binary: 'tapid.exe', runner: 'windows-latest' },
  { target: 'x86_64-unknown-linux-gnu', binary: 'tapid', runner: 'ubuntu-latest' },
] as const;
export type ReleaseTarget = typeof PLATFORMS[number]['target'];
export const TARGETS: readonly ReleaseTarget[] = PLATFORMS.map(platform => platform.target);
