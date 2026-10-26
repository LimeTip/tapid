import { publicationPlan, type CargoMetadata, type Package } from './publish.ts';
import { PublicTransport } from './public_transport.ts';

export type PublicPackage = Package & { yanked: false };

/** Strict read-back of the full tagged publication plan, including existing versions. */
export async function verifyPublicPackages(transport: PublicTransport, metadata: CargoMetadata): Promise<PublicPackage[]> {
  const names = new Set<string>();
  for (const pkg of metadata.packages) {
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(pkg.name) || names.has(pkg.name)) throw Error('invalid or duplicate tagged package name');
    names.add(pkg.name);
    // Cargo has parsed SemVer already; constrain the URL component without
    // applying the stable release-tag policy to independent crate versions.
    if (!/^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?$/.test(pkg.version)) throw Error('invalid tagged package version');
  }
  const packages = publicationPlan(metadata, new Set());
  const result: PublicPackage[] = [];
  for (const pkg of packages) {
    const response = await transport.json(`https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}`);
    const version = response && typeof response === 'object' && !Array.isArray(response)
      ? (response as Record<string, unknown>).version : undefined;
    if (!version || typeof version !== 'object' || Array.isArray(version)) throw Error(`malformed public crate version: ${pkg.name}@${pkg.version}`);
    const value = version as Record<string, unknown>;
    if (value.crate !== pkg.name || value.num !== pkg.version || value.yanked !== false) throw Error(`public crate version missing, inconsistent, or yanked: ${pkg.name}@${pkg.version}`);
    result.push({ ...pkg, yanked: false });
  }
  return result;
}
