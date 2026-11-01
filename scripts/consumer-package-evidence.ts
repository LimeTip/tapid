// Placement-bound evidence, compared by logical identity rather than physical hoisting.
import { readFileSync, existsSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
type Json = Record<string, any>;
const read = (path: string): Json => JSON.parse(readFileSync(path, "utf8"));
const canonical = (value: any): string =>
  JSON.stringify(value, (_key, item) =>
    item && typeof item === "object" && !Array.isArray(item)
      ? Object.fromEntries(
          Object.entries(item).sort(([a], [b]) => a.localeCompare(b)),
        )
      : item,
  );
function provider(records: Json, parent: string, name: string): string | null {
  let current = parent;
  while (true) {
    const candidate = current
      ? current + "/node_modules/" + name
      : "node_modules/" + name;
    if (records[candidate]) return candidate;
    if (!current) return null;
    const i = current.lastIndexOf("/node_modules/");
    current = i < 0 ? "" : current.slice(0, i);
  }
}
export function resolveInstalledDependency(
  root: string,
  parent: string,
  name: string,
): string | null {
  let dir = parent;
  while (dir === root || dir.startsWith(root + sep)) {
    const path = join(dir, "node_modules", name, "package.json");
    if (existsSync(path) && statSync(path).isFile()) return path;
    if (dir === root) break;
    dir = dirname(dir);
  }
  return null;
}
function evidence(root: string, npm: boolean) {
  const lock = read(join(root, npm ? "package-lock.json" : "tapid.lock"));
  const native = !npm && lock.lockfileVersion === 7;
  if (!npm && ![7, 8].includes(lock.lockfileVersion))
    throw new Error("unsupported Tapid schema");
  const records: Json = native
    ? lock.packages
    : (npm ? lock : lock.npmLock).packages;
  if (
    !records ||
    (!native && (npm ? lock : lock.npmLock).lockfileVersion !== 3)
  )
    throw new Error("unsupported npm schema");
  const errors: string[] = [],
    omitted: Json[] = [],
    unsupported: Json[] = [];
  const instances: Json[] = [];
  const retainedInapplicable: Json[] = [];
  const seen = new Set<string>(),
    nodes = new Set<string>(),
    edges = new Set<string>();
  const tokens = new Map<string, Json>();
  const token = (id: string): Json => {
    if (tokens.has(id)) return tokens.get(id)!;
    const r = records[id];
    if (!r) {
      errors.push("missing lock record: " + id);
      return {};
    }
    const name = r.name ?? id.split("node_modules/").at(-1);
    const url = native ? r.artifactUrl : r.resolved;
    const integrity = native ? r.artifactIntegrity : r.integrity;
    if (!url || !integrity) errors.push("missing provenance: " + id);
    if (
      r.link ||
      r.inBundle ||
      r.bundleDependencies ||
      (url && !url.startsWith("https://"))
    )
      unsupported.push({ id, reason: "non-registry or bundled dependency" });
    const registry = native ? r.registry : url ? new URL(url).origin : null;
    if (native && r.registryIntegrityDeclared === false)
      errors.push("registry integrity undeclared: " + id);
    const value = { name, version: r.version, url, registry, integrity };
    tokens.set(id, value);
    return value;
  };
  const target = (parent: string, name: string): string | null => {
    if (!native) return provider(records, parent, name);
    if (parent) return records[parent]?.dependencies?.[name] ?? null;
    return (
      lock.rootBindings?.[name] ??
      (lock.roots ?? []).find((id: string) => records[id]?.name === name) ??
      null
    );
  };
  const peerTarget = (
    id: string,
    parentPath: string,
    name: string,
  ): string | null => {
    if (!native) return target(id, name);
    const path = resolveInstalledDependency(root, parentPath, name);
    if (!path) return null;
    const actual = read(path);
    const context = (records[id].peerContext ?? "")
      .split(",")
      .find(
        (entry: string) =>
          entry.startsWith("name=") &&
          decodeURIComponent(entry.slice(5).split(";version=")[0]) === name,
      );
    if (
      context &&
      decodeURIComponent(context.split(";version=")[1]) !== actual.version
    )
      errors.push("peer context differs: " + id + " -> " + name);
    const candidates = Object.keys(records).filter(
      (key) =>
        records[key].name === actual.name &&
        records[key].version === actual.version,
    );
    if (candidates.length !== 1) {
      errors.push("ambiguous peer provenance: " + id + " -> " + name);
      return null;
    }
    return candidates[0];
  };
  const rootManifest = read(join(root, "package.json"));
  const applicable = new Set<string>();
  const inapplicableBranch = new Set<string>();
  if (!native) {
    const hostEntries = Object.entries({
      os: process.platform,
      cpu: process.arch,
      libc:
        process.platform === "linux" &&
        (process.report.getReport() as any).header.glibcVersionRuntime
          ? "glibc"
          : "",
    });
    const incompatible = (r: Json) =>
      hostEntries.some(([f, value]) => {
        const list = r[f] ?? [];
        return (
          list.includes("!" + value) ||
          (list.some((v: string) => !v.startsWith("!")) &&
            !list.includes(value))
        );
      });
    const pending: [string, Json][] = [["", rootManifest]];
    for (let i = 0; i < pending.length; i++) {
      const [parent, r] = pending[i];
      const deps = {
        ...r.dependencies,
        ...(!parent ? r.devDependencies : {}),
        ...r.optionalDependencies,
        ...r.peerDependencies,
      };
      for (const name of Object.keys(deps)) {
        const id = provider(records, parent, name);
        if (!id || applicable.has(id)) continue;
        if (incompatible(records[id])) {
          const branch = [id];
          for (let j = 0; j < branch.length; j++) {
            const child = branch[j];
            if (inapplicableBranch.has(child)) continue;
            inapplicableBranch.add(child);
            const record = records[child];
            for (const name of Object.keys({
              ...record.dependencies,
              ...record.optionalDependencies,
            })) {
              const next = provider(records, child, name);
              if (next) branch.push(next);
            }
          }
          continue;
        }
        applicable.add(id);
        pending.push([id, records[id]]);
      }
    }
  }
  function logical(id: string, path: string) {
    const manifest = read(path);
    const peers = Object.fromEntries(
      Object.keys(manifest.peerDependencies ?? {})
        .sort()
        .map((name) => {
          const selected = peerTarget(id, dirname(path), name);
          return [name, selected ? token(selected) : null];
        }),
    );
    return {
      identity: token(id),
      peers,
      os: manifest.os ?? [],
      cpu: manifest.cpu ?? [],
      libc: manifest.libc ?? [],
    };
  }
  const queue: { id: string; path: string }[] = [];
  function walk(parentId: string, parentPath: string, manifest: Json) {
    const requirements = {
      ...manifest.dependencies,
      ...manifest.devDependencies,
      ...manifest.optionalDependencies,
    };
    // Dev dependencies only belong to the project root.
    if (parentId) {
      for (const name of Object.keys(manifest.devDependencies ?? {}))
        if (
          !(name in (manifest.dependencies ?? {})) &&
          !(name in (manifest.optionalDependencies ?? {}))
        )
          delete requirements[name];
    }
    for (const [kind, deps] of [
      ["dependency", requirements],
      ["peer", manifest.peerDependencies ?? {}],
    ] as const) {
      for (const [name, range] of Object.entries(deps)) {
        const selected =
          kind === "peer" && parentId
            ? peerTarget(parentId, parentPath, name)
            : target(parentId, name);
        const actual = resolveInstalledDependency(root, parentPath, name);
        const optional =
          name in (manifest.optionalDependencies ?? {}) ||
          (kind === "peer" && manifest.peerDependenciesMeta?.[name]?.optional);
        if (!actual) {
          if (!optional)
            errors.push(`missing required ${parentId || "<root>"} -> ${name}`);
          edges.add(
            canonical({
              parent: parentId
                ? logical(parentId, join(parentPath, "package.json"))
                : "<root>",
              name,
              range,
              kind,
              provider: null,
            }),
          );
          continue;
        }
        if (!selected) {
          errors.push(`missing selected lock edge ${parentId} -> ${name}`);
          continue;
        }
        const identity = token(selected),
          installed = read(actual);
        if (
          identity.name !== installed.name ||
          identity.version !== installed.version
        )
          errors.push("installed identity differs: " + actual);
        edges.add(
          canonical({
            parent: parentId
              ? logical(parentId, join(parentPath, "package.json"))
              : "<root>",
            name,
            range,
            kind,
            provider: identity,
          }),
        );
        queue.push({ id: selected, path: actual });
      }
    }
  }
  walk("", root, rootManifest);
  for (let i = 0; i < queue.length; i++) {
    const { id, path } = queue[i];
    const visit = id + ":" + path;
    if (seen.has(visit)) continue;
    seen.add(visit);
    const manifest = read(path),
      identity = token(id);
    const peers = Object.fromEntries(
      Object.keys(manifest.peerDependencies ?? {})
        .sort()
        .map((name) => {
          const selected = peerTarget(id, dirname(path), name);
          return [name, selected ? token(selected) : null];
        }),
    );
    nodes.add(canonical(logical(id, path)));
    if (!native)
      for (const field of [
        "os",
        "cpu",
        "libc",
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "peerDependenciesMeta",
      ]) {
        const expected =
          records[id][field] ??
          (!["os", "cpu", "libc"].includes(field) ? {} : []);
        let actual = manifest[field] ?? (Array.isArray(expected) ? [] : {});
        if (field === "dependencies")
          actual = Object.fromEntries(
            Object.entries(actual).filter(
              ([name]) => !(name in (manifest.optionalDependencies ?? {})),
            ),
          );
        if (canonical(expected) !== canonical(actual))
          errors.push(
            "lock constraints differ from installed manifest: " +
              id +
              " " +
              field,
          );
      }
    if (!npm && !native) {
      const receipt = lock.verifiedArtifacts?.[id];
      if (
        !receipt?.treeDigest ||
        receipt.resolved !== identity.url ||
        receipt.integrity !== identity.integrity
      )
        errors.push("missing or mismatched verification receipt: " + id);
    }
    instances.push({
      placement: relative(root, dirname(path)).split(sep).join("/"),
      lockPlacement: id,
      identity,
      peers,
      registryIntegrityDeclared: native
        ? records[id].registryIntegrityDeclared
        : true,
      verificationReceipt:
        !npm && !native
          ? lock.verifiedArtifacts?.[id]
          : native
            ? {
                treeDigest: records[id].treeDigest,
                integrity: records[id].artifactIntegrity,
                resolved: records[id].artifactUrl,
              }
            : null,
    });
    walk(id, dirname(path), manifest);
  }
  if (!native)
    for (const [id, r] of Object.entries(records) as [string, Json][]) {
      if (!id || [...seen].some((v) => v.startsWith(id + ":"))) continue;
      const constraints = Object.fromEntries(
        ["os", "cpu", "libc"].map((f) => [f, r[f] ?? []]),
      );
      const installed = existsSync(join(root, id, "package.json"));
      const permitted =
        r.optional === true &&
        !applicable.has(id) &&
        inapplicableBranch.has(id);
      const inventory = installed ? retainedInapplicable : omitted;
      inventory.push({
        id,
        ...token(id),
        constraints,
        permitted,
      });
      if (!permitted && !installed)
        errors.push("unexplained omitted lock package: " + id);
    }
  return {
    nodes: [...nodes].sort(),
    edges: [...edges].sort(),
    instances,
    omitted,
    retainedInapplicable,
    unsupported,
    errors,
  };
}
export function compareEvidence(npmRoot: string, tapidRoot: string) {
  const tapid = evidence(tapidRoot, false);
  const npm = evidence(npmRoot, true)!;
  const differences: Json[] = [];
  for (const field of ["nodes", "edges"] as const)
    for (const [manager, other] of [
      [npm, tapid],
      [tapid, npm],
    ]) {
      for (const value of manager[field])
        if (!other[field].includes(value))
          differences.push({
            field,
            manager: manager === npm ? "npm" : "tapid",
            value: JSON.parse(value),
          });
    }
  return {
    npm,
    tapid,
    differences,
    errors: [
      ...npm.errors,
      ...tapid.errors,
      ...npm.unsupported.map(canonical),
      ...tapid.unsupported.map(canonical),
    ],
  };
}
