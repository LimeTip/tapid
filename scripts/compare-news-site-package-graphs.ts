// Compare installed npm and Tapid graphs, including peer edges and provenance.
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { machine, type } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

type Json = Record<string, any>;
type Pair = [string, string];
type Instance = { path: string; manifest: Json; manifestPath: string };
type Nodes = Map<string, Instance[]>;
const key = (value: unknown) => JSON.stringify(value);
const pair = (value: Json): Pair | null =>
  typeof value.name === "string" && typeof value.version === "string"
    ? [value.name, value.version]
    : null;
const difference = (a: Set<string>, b: Set<string>) =>
  new Set([...a].filter((item) => !b.has(item)));
const intersection = (a: Set<string>, b: Set<string>) =>
  new Set([...a].filter((item) => b.has(item)));
const sorted = (values: Iterable<string>): any[] =>
  [...values]
    .map((value) => JSON.parse(value))
    .sort((a, b) => {
      for (let i = 0; i < Math.max(a.length, b.length); i++) {
        if (a[i] < b[i]) return -1;
        if (a[i] > b[i]) return 1;
      }
      return 0;
    });
const rows = (values: Set<string>) =>
  sorted(values).map(([name, version]) => ({ name, version }));
const edgeRows = (values: Set<string>) =>
  sorted(values).map((edge) => ({
    parent: { name: edge[0], version: edge[1] },
    dependency: edge[2],
    range: edge[3],
    kind: edge[4],
    provider:
      edge[5] === "<missing>" ? null : { name: edge[5], version: edge[6] },
  }));
function readJson(path: string): Json {
  try {
    const value = JSON.parse(readFileSync(path, "utf8"));
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("expected a JSON object");
    return value;
  } catch (error) {
    throw new Error(`cannot read JSON from ${path}: ${error}`);
  }
}
function installedNodes(project: string): Nodes {
  const modules = join(project, "node_modules");
  if (!existsSync(modules) || !statSync(modules).isDirectory())
    throw new Error(`missing installed node_modules tree: ${modules}`);
  const result: Nodes = new Map();
  let count = 0;
  function visit(directory: string, scope = false) {
    for (const entry of readdirSync(directory, { withFileTypes: true }).sort(
      (a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
    )) {
      if (entry.name.startsWith(".")) continue;
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink())
        throw new Error(`unsupported installed package link: ${path}`);
      if (!entry.isDirectory()) continue;
      if (!scope && entry.name.startsWith("@")) {
        visit(path, true);
        continue;
      }
      if (++count > 100_000) throw new Error("installed package traversal limit exceeded");
      const manifestPath = join(path, "package.json"), manifest = readJson(manifestPath);
      const identity = pair(manifest);
      if (!identity) throw new Error(`invalid installed package identity: ${manifestPath}`);
      const id = key(identity), instance = {
        path: relative(modules, path).split(sep).join("/"), manifest, manifestPath,
      };
      result.set(id, [...(result.get(id) ?? []), instance]);
      const nested = join(path, "node_modules");
      if (existsSync(nested)) visit(nested);
    }
  }
  visit(modules);
  return result;
}
function resolveChild(
  project: string,
  parent: string,
  name: string,
): string | null {
  let current = parent;
  while (current === project || current.startsWith(project + sep)) {
    const candidate = join(current, "node_modules", name, "package.json");
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    if (current === project) break;
    current = dirname(current);
  }
  return null;
}
function edges(
  manifest: Json,
  root = false,
): [string, string, string, boolean][] {
  const kinds = root
    ? [
        ["dependencies", "dependency"],
        ["devDependencies", "devDependency"],
        ["optionalDependencies", "optionalDependency"],
        ["peerDependencies", "peerDependency"],
      ]
    : [
        ["dependencies", "dependency"],
        ["optionalDependencies", "optionalDependency"],
      ];
  return kinds.flatMap(([field, kind]) =>
    Object.entries(manifest[field] ?? {}).map(
      ([name, requirement]) =>
        [
          name,
          String(requirement),
          kind,
          kind === "optionalDependency" ||
            (kind === "peerDependency" &&
              Boolean(manifest.peerDependenciesMeta?.[name]?.optional)),
        ] as [string, string, string, boolean],
    ),
  );
}
function reachableGraph(project: string) {
  const manifest = readJson(join(project, "package.json")),
    rootPair: Pair = pair(manifest) ?? ["<root>", ""];
  const queue: [string, Pair, string, string, string, boolean][] = edges(
    manifest,
    true,
  ).map((edge) => [project, rootPair, ...edge]);
  const visited = new Set<string>(),
    pairs = new Set<string>(),
    dependencyEdges = new Set<string>(),
    peerEdges = new Set<string>(),
    missingRequired = new Set<string>();
  for (let index = 0; index < queue.length; index++) {
    if (index >= 100_000) throw new Error("reachable graph traversal limit exceeded");
    const [parent, parentPair, name, requirement, kind, optional] =
        queue[index],
      id = key([parent, name, kind, requirement]);
    if (visited.has(id)) continue;
    visited.add(id);
    const childPath = resolveChild(project, parent, name),
      child = childPath ? readJson(childPath) : null,
      identity = child ? pair(child) : null;
    const childDirectory = childPath ? dirname(childPath) : parent;
    if (identity) pairs.add(key(identity));
    else if (!childPath && !optional)
      missingRequired.add(key([parentPair.join("@"), name, requirement]));
    dependencyEdges.add(
      key([
        ...parentPair,
        name,
        requirement,
        kind,
        ...(identity ?? ["<missing>", ""]),
      ]),
    );
    if (!child) continue;
    const current: Pair = identity ?? ["<invalid>", ""];
    for (const edge of edges(child))
      queue.push([childDirectory, current, ...edge]);
    for (const [peerName, peerRequirement] of Object.entries(
      child.peerDependencies ?? {},
    )) {
      const peerOptional = Boolean(
          child.peerDependenciesMeta?.[peerName]?.optional,
        ),
        providerPath = resolveChild(project, childDirectory, peerName),
        provider = providerPath ? pair(readJson(providerPath)) : null;
      if (provider) {
        pairs.add(key(provider));
        queue.push([
          childDirectory,
          current,
          peerName,
          String(peerRequirement),
          "peerProvider",
          true,
        ]);
      } else if (!peerOptional)
        missingRequired.add(
          key([current.join("@"), peerName, String(peerRequirement)]),
        );
      peerEdges.add(
        key([
          ...current,
          peerName,
          String(peerRequirement),
          peerOptional ? "optional" : "required",
          ...(provider ?? ["<missing>", ""]),
        ]),
      );
    }
  }
  return { pairs, dependencyEdges, peerEdges, missingRequired };
}
function registryOrigin(value: unknown): string | null {
  if (typeof value !== "string" || !value) return null;
  try {
    const url = new URL(value);
    return url.host
      ? url.protocol.toLowerCase() + "//" + url.host.toLowerCase()
      : null;
  } catch {
    return null;
  }
}

// Refine both graphs together so class numbers have the same meaning on each
// side. Including the previous class makes every round a partition refinement;
// an unchanged class count therefore proves a fixed point, including cycles.
function contextualComparison(projects: string[]) {
  type Vertex = { path: string; identity: Pair; provenance: unknown; bindings: { label: string; target: number | null }[] };
  const vertices: Vertex[] = [], roots: number[] = [];
  const nativeEvaluation = readJson(join(projects[1], "tapid.lock")).lockfileVersion !== 8;
  const unavailable: { side: number; path: string; reason: string }[] = [];
  for (const [side, project] of projects.entries()) {
    const lock = readJson(join(project, side === 0 ? "package-lock.json" : "tapid.lock"));
    const placements = side === 0 ? lock : lock.lockfileVersion === 8 ? lock.npmLock : null;
    const seen = new Map<string, number>();
    const queue: { directory: string; index: number; root: boolean }[] = [];
    function intern(directory: string, root = false): number {
      directory = realpathSync(directory);
      const existing = seen.get(directory);
      if (existing !== undefined) return existing;
      if (vertices.length >= 10_000) throw new Error("contextual graph size limit exceeded");
      const manifest = readJson(join(directory, "package.json"));
      const identity = pair(manifest) ?? (root ? ["<root>", ""] as Pair : null);
      if (!identity) throw new Error(`invalid installed package identity: ${directory}`);
      const index = vertices.length;
      let provenance: unknown = null;
      if (!root) {
        const placement = relative(project, directory).split(sep).join("/");
        const candidates: Json[] = placements
          ? [placements.packages?.[placement]].filter(Boolean)
          : (Object.values(lock.packages ?? {}) as Json[]).filter((record) =>
            record?.name === identity[0] && record?.version === identity[1]);
        if (candidates.length !== 1)
          unavailable.push({ side, path: placement, reason: "missing or ambiguous lock association" });
        else {
          const record = candidates[0];
          provenance = placements
            ? [registryOrigin(record.resolved), record.resolved, record.integrity]
            : [registryOrigin(record.registry), record.artifactUrl, record.artifactIntegrity];
          if (record.version !== identity[1] || (record.name && record.name !== identity[0]) ||
            (provenance as unknown[]).some((value) => typeof value !== "string" || !value))
            unavailable.push({ side, path: placement, reason: "incomplete or mismatched lock identity" });
          // Native contexts describe one selected platform, not npm's original
          // constraint lists. Only compare a singleton positive constraint to
          // such a context; richer lists must remain unavailable evidence.
          const platform = placements
            ? ["os", "cpu", "libc"].map((field) => {
                const values = record[field] ?? [];
                if (!Array.isArray(values) || values.some((value: unknown) => typeof value !== "string"))
                  throw new Error(`invalid platform constraint at ${placement}: ${field}`);
                if (nativeEvaluation && (values.length > 1 ||
                  values.some((value: string) => value.startsWith("!")))) {
                  unavailable.push({ side, path: placement, reason: "platform constraint needs richer comparison" });
                }
                return [...values].sort();
              })
            : ["os", "cpu", "libc"].map((field) => {
                const value = String(record.platformContext ?? "")
                  .split(";").find((part) => part.startsWith(field + "="))?.slice(field.length + 1);
                return value ? [decodeURIComponent(value)] : [];
              });
          provenance = [provenance, platform];
        }
      }
      seen.set(directory, index);
      vertices.push({ path: relative(project, directory).split(sep).join("/") || ".", identity, provenance, bindings: [] });
      queue.push({ directory, index, root });
      return index;
    }
    roots.push(intern(project, true));
    for (let cursor = 0; cursor < queue.length; cursor++) {
      const { directory, index, root } = queue[cursor];
      const manifest = readJson(join(directory, "package.json"));
      const declarations = edges(manifest, root);
      if (!root)
        for (const [name, range] of Object.entries(manifest.peerDependencies ?? {}))
          declarations.push([name, String(range), "peerDependency",
            Boolean(manifest.peerDependenciesMeta?.[name]?.optional)]);
      vertices[index].bindings = declarations.map(([name, range, kind, optional]) => {
        const provider = resolveChild(project, directory, name);
        return { label: key([name, range, kind, optional]),
          target: provider ? intern(dirname(provider)) : null };
      }).sort((a, b) => a.label.localeCompare(b.label));
    }
  }
  const classify = (signatures: string[]) => {
    const classes = new Map([...new Set(signatures)].sort().map((signature, index) => [signature, index]));
    return signatures.map((signature) => classes.get(signature)!);
  };
  let colors = classify(vertices.map((vertex) => key([vertex.identity, vertex.provenance])));
  for (let round = 0; round <= vertices.length; round++) {
    const next = classify(vertices.map((vertex, index) => key([colors[index],
      vertex.bindings.map(({ label, target }) => [label, target === null ? null : colors[target]])])));
    const stable = new Set(next).size === new Set(colors).size;
    colors = next;
    if (stable) return { equal: !unavailable.length && colors[roots[0]] === colors[roots[1]], unavailable,
      instances: vertices.map((vertex, index) => ({ ...vertex, context: colors[index] })), roots };
  }
  throw new Error("contextual graph refinement did not converge");
}
function lockRecords(
  project: string,
  nodes: Nodes,
  npm: boolean,
): Map<string, Json[]> {
  const result = new Map<string, Json[]>(),
    lock = readJson(join(project, npm ? "package-lock.json" : "tapid.lock"));
  if (!npm && lock.lockfileVersion === 8) {
    if (lock.npmLock?.lockfileVersion !== 3)
      throw new Error("imported Tapid lock must contain npm v3 placement evidence");
    for (const [id, instances] of nodes)
      for (const instance of instances) {
        const record = lock.npmLock.packages?.["node_modules/" + instance.path];
        result.set(id, [...(result.get(id) ?? []), record && record.version === instance.manifest.version ? {
            ...record, name: instance.manifest.name,
            registry: registryOrigin(record.resolved),
            artifactUrl: record.resolved, artifactIntegrity: record.integrity,
            platformContext: record.optional === true &&
              ["os", "cpu", "libc"].some((field) => record[field]?.length)
              ? "imported-platform" : "",
          } : {}]);
      }
    return result;
  }
  if (npm) {
    if (lock.lockfileVersion !== 3)
      throw new Error(
        `npm reference must use lockfileVersion 3, got ${lock.lockfileVersion}`,
      );
    for (const [id, instances] of nodes)
      for (const instance of instances) {
        const record = lock.packages?.["node_modules/" + instance.path];
        result.set(id, [...(result.get(id) ?? []),
          record && record.version === instance.manifest.version ? record : {}]);
      }
  } else
    for (const record of Object.values(lock.packages ?? {}) as Json[]) {
      if (!record || typeof record !== "object") continue;
      const identity = pair(record);
      if (identity)
        result.set(key(identity), [
          ...(result.get(key(identity)) ?? []),
          record,
        ]);
    }
  return result;
}
function packageMetadata(
  nodes: Nodes,
  records: Map<string, Json[]>,
  npm: boolean,
) {
  const result = new Map<
    string,
    { sources: Set<string>; artifacts: Set<string>; integrities: Set<string>; platform: boolean; missing: Set<string> }
  >();
  for (const id of nodes.keys()) {
    const matching = records.get(id) ?? [];
    const missing = new Set<string>();
    for (const [field, property] of [
      ["source", npm ? "resolved" : "registry"],
      ["artifactUrl", npm ? "resolved" : "artifactUrl"],
      ["integrity", npm ? "integrity" : "artifactIntegrity"],
    ])
      if (!matching.length || matching.some((record) =>
        typeof record[property] !== "string" || !record[property])) missing.add(field);
    const sources = new Set(
      matching
        .map((record) => registryOrigin(record[npm ? "resolved" : "registry"]))
        .filter((value) => value !== null) as string[],
    );
    const integrities = new Set(
      matching
        .map((record) => record[npm ? "integrity" : "artifactIntegrity"])
        .filter((value) => typeof value === "string"),
    );
    const artifacts = new Set<string>(matching
      .map((record) => record[npm ? "resolved" : "artifactUrl"])
      .filter((value) => typeof value === "string" && value.length > 0));
    const platform = matching.some((record) =>
      npm
        ? record.optional === true &&
          ["os", "cpu", "libc"].some((field) => record[field]?.length)
        : Boolean(record.platformContext) &&
          record.platformContext !== "os=;cpu=;libc=",
    );
    result.set(id, { sources, artifacts, integrities, platform, missing });
  }
  return result;
}
export function compare(npmRoot: string, tapidRoot: string) {
  npmRoot = realpathSync(npmRoot);
  tapidRoot = realpathSync(tapidRoot);
  const npmNodes = installedNodes(npmRoot),
    tapidNodes = installedNodes(tapidRoot),
    npmGraph = reachableGraph(npmRoot),
    tapidGraph = reachableGraph(tapidRoot);
  const contexts = contextualComparison([npmRoot, tapidRoot]);
  const npmMeta = packageMetadata(
      npmNodes,
      lockRecords(npmRoot, npmNodes, true),
      true,
    ),
    tapidMeta = packageMetadata(
      tapidNodes,
      lockRecords(tapidRoot, tapidNodes, false),
      false,
    );
  const npmPairs = new Set(npmNodes.keys()),
    tapidPairs = new Set(tapidNodes.keys()),
    npmReachable = npmGraph.pairs,
    tapidReachable = tapidGraph.pairs;
  const sourceMismatches: Json[] = [],
    artifactUrlMismatches: Json[] = [],
    integrityMismatches: Json[] = [],
    metadataMissing: Json[] = [];
  for (const identity of sorted(new Set([...npmReachable, ...tapidReachable]))) {
    const id = key(identity),
      baseline = npmMeta.get(id),
      evaluated = tapidMeta.get(id);
    for (const [field, metadata, mismatches] of [
      ["source", "sources", sourceMismatches],
      ["artifactUrl", "artifacts", artifactUrlMismatches],
      ["integrity", "integrities", integrityMismatches],
    ] as const) {
      const before: Set<string> = baseline?.[metadata] ?? new Set(),
        after: Set<string> = evaluated?.[metadata] ?? new Set();
      if (!before.size || !after.size || baseline?.missing.has(field) || evaluated?.missing.has(field))
        metadataMissing.push({
          name: identity[0],
          version: identity[1],
          field,
        });
      else if (difference(before, after).size || difference(after, before).size)
        mismatches.push({
          name: identity[0],
          version: identity[1],
          npm: [...before].sort(),
          tapid: [...after].sort(),
        });
    }
  }
  const npmPlatform = new Set(
      [...npmReachable].filter((id) => npmMeta.get(id)?.platform),
    ),
    tapidPlatform = new Set(
      [...tapidReachable].filter((id) => tapidMeta.get(id)?.platform),
    );
  const pairDifference = (a: Set<string>, b: Set<string>) => ({
    npmOnly: rows(difference(a, b)),
    tapidOnly: rows(difference(b, a)),
  });
  const edgeDifference = (a: Set<string>, b: Set<string>) => ({
    npmOnly: edgeRows(difference(a, b)),
    tapidOnly: edgeRows(difference(b, a)),
  });
  const provenance = (
    metadata: ReturnType<typeof packageMetadata>,
    id: string,
  ) => ({
    registryOrigins: [...(metadata.get(id)?.sources ?? [])].sort(),
    artifactUrls: [...(metadata.get(id)?.artifacts ?? [])].sort(),
    integrities: [...(metadata.get(id)?.integrities ?? [])].sort(),
  });
  const report = {
    schemaVersion: 2,
    baseline: "npm",
    evaluated: "Tapid",
    contextualGraphEqual: contexts.equal,
    contextualGraph: contexts,
    verificationReceipts: {
      tapid: (() => {
        const lock = readJson(join(tapidRoot, "tapid.lock"));
        return { trees: Object.keys(lock.verifiedTrees ?? {}).length,
          artifacts: Object.keys(lock.verifiedArtifacts ?? {}).length };
      })(),
    },
    environment: {
      system: type(),
      machine: machine(),
      libc:
        process.platform === "linux" &&
        (process.report.getReport() as any).header.glibcVersionRuntime
          ? "glibc"
          : "",
    },
    counts: {
      npmPhysicalPackages: npmPairs.size,
      tapidPhysicalPackages: tapidPairs.size,
      npmReachablePackages: npmReachable.size,
      tapidReachablePackages: tapidReachable.size,
      npmLockRecords:
        Object.keys(readJson(join(npmRoot, "package-lock.json")).packages ?? {})
          .length - 1,
      tapidLockRecords: Object.keys(
        readJson(join(tapidRoot, "tapid.lock")).packages ?? {},
      ).length,
    },
    graphSnapshot: {
      reachablePackages: {
        npm: rows(npmReachable),
        tapid: rows(tapidReachable),
      },
      dependencyEdges: {
        npm: edgeRows(npmGraph.dependencyEdges),
        tapid: edgeRows(tapidGraph.dependencyEdges),
      },
      peerEdges: {
        npm: edgeRows(npmGraph.peerEdges),
        tapid: edgeRows(tapidGraph.peerEdges),
      },
      platformOptionalPackages: {
        npm: rows(npmPlatform),
        tapid: rows(tapidPlatform),
      },
      provenance: sorted(new Set([...npmPairs, ...tapidPairs])).map(
        ([name, version]) => ({
          name,
          version,
          npm: provenance(npmMeta, key([name, version])),
          tapid: provenance(tapidMeta, key([name, version])),
        }),
      ),
    },
    reachablePackageDifferences: pairDifference(npmReachable, tapidReachable),
    physicalPackageDifferences: pairDifference(npmPairs, tapidPairs),
    unreachablePhysicalPackages: {
      npm: rows(difference(npmPairs, npmReachable)),
      tapid: rows(difference(tapidPairs, tapidReachable)),
    },
    dependencyEdgeDifferences: edgeDifference(
      npmGraph.dependencyEdges,
      tapidGraph.dependencyEdges,
    ),
    peerEdgeDifferences: edgeDifference(
      npmGraph.peerEdges,
      tapidGraph.peerEdges,
    ),
    platformOptionalPackageDifferences: pairDifference(
      npmPlatform,
      tapidPlatform,
    ),
    sourceIdentityMismatches: sourceMismatches,
    artifactUrlMismatches,
    integrityMismatches,
    missingProvenance: metadataMissing,
    missingRequiredEdges: {
      npm: sorted(npmGraph.missingRequired),
      tapid: sorted(tapidGraph.missingRequired),
    },
  };
  const mismatch =
    !contexts.equal ||
    [
      report.reachablePackageDifferences,
      report.dependencyEdgeDifferences,
      report.peerEdgeDifferences,
      report.platformOptionalPackageDifferences,
    ].some((value) => value.npmOnly.length || value.tapidOnly.length) ||
    sourceMismatches.length ||
    artifactUrlMismatches.length ||
    integrityMismatches.length ||
    metadataMissing.length ||
    npmGraph.missingRequired.size ||
    tapidGraph.missingRequired.size;
  const names = (values: Set<string>) =>
    sorted(values)
      .map(([name, version]) => `${name}@${version}`)
      .join(", ") || "none";
  const text = [
    "# News-site dependency graph comparison",
    "Baseline: npm; evaluated package manager: Tapid.",
    `Contextual graph equality: ${contexts.equal}.`,
    `Unavailable instance associations: ${contexts.unavailable.length}.`,
    `Reachable package identities: npm ${npmReachable.size}, Tapid ${tapidReachable.size}.`,
    `Physical installed identities: npm ${npmPairs.size}, Tapid ${tapidPairs.size}.`,
    `Reachable package differences: npm-only ${report.reachablePackageDifferences.npmOnly.length}, Tapid-only ${report.reachablePackageDifferences.tapidOnly.length}.`,
    `Dependency-edge differences: npm-only ${report.dependencyEdgeDifferences.npmOnly.length}, Tapid-only ${report.dependencyEdgeDifferences.tapidOnly.length}.`,
    `Peer-edge differences: npm-only ${report.peerEdgeDifferences.npmOnly.length}, Tapid-only ${report.peerEdgeDifferences.tapidOnly.length}.`,
    `Platform-optional package differences: npm-only ${report.platformOptionalPackageDifferences.npmOnly.length}, Tapid-only ${report.platformOptionalPackageDifferences.tapidOnly.length}.`,
    "Shared platform-optional packages: " +
      names(intersection(npmPlatform, tapidPlatform)),
    `Source mismatches: ${sourceMismatches.length}; integrity mismatches: ${integrityMismatches.length}; missing provenance fields: ${metadataMissing.length}.`,
    `Artifact URL mismatches: ${artifactUrlMismatches.length}.`,
    "",
    "## Physical-only packages (reported even when unreachable)",
    "npm-only: " + names(difference(npmPairs, tapidPairs)),
    "Tapid-only: " + names(difference(tapidPairs, npmPairs)),
    "",
  ].join("\n");
  return { report, text, code: mismatch ? 1 : 0 };
}
function main(): number {
  try {
    const { values } = parseArgs({
      options: {
        "npm-root": { type: "string" },
        "tapid-root": { type: "string" },
        json: { type: "string" },
        text: { type: "string" },
      },
    });
    if (!values["npm-root"] || !values["tapid-root"])
      throw new Error("--npm-root and --tapid-root are required");
    const { report, text, code } = compare(
      values["npm-root"],
      values["tapid-root"],
    );
    const encoded = JSON.stringify(report, null, 2) + "\n";
    for (const [path, contents] of [
      [values.json, encoded],
      [values.text, text],
    ])
      if (path) {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, contents);
      }
    if (!values.json) process.stdout.write(encoded);
    process.stderr.write(text);
    return code;
  } catch (error) {
    console.error(`graph comparison error: ${error}`);
    return 2;
  }
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  process.exitCode = main();
