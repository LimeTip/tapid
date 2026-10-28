import { createHash } from "node:crypto";
import { readFileSync, realpathSync, openSync, closeSync } from "node:fs";
import { spawn } from "node:child_process";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

/** Runs one command without a shell. Timeout termination is never Ctrl+C evidence.
 * Native tree ownership must be established separately by the CLI receipt.
 * Unlike the documentation runner's boundedProcess, evidence needs separate,
 * untruncated, exclusive log files rather than a merged in-memory summary. */
export async function collectCommand(options: {
  executable: string; argv: string[]; cwd: string; directory: string;
  name: string; timeoutMs: number; env: NodeJS.ProcessEnv;
}) {
  if (!/^[a-z][a-z0-9-]*$/.test(options.name) ||
      !Number.isFinite(options.timeoutMs) || options.timeoutMs <= 0)
    throw new Error("safe command name and positive timeout required");
  const stdout = `${options.name}.stdout.log`, stderr = `${options.name}.stderr.log`;
  const out = openSync(resolve(options.directory, stdout), "wx", 0o600);
  let err: number;
  try { err = openSync(resolve(options.directory, stderr), "wx", 0o600); }
  catch (error) { closeSync(out); throw error; }
  const started = performance.now();
  let error: string | null = null, timedOut = false;
  try {
    const child = spawn(options.executable, options.argv, {
      cwd: options.cwd, env: options.env, stdio: ["ignore", out, err], shell: false,
    });
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, options.timeoutMs);
    const completion = await new Promise<{ exit: number | null; signal: string | null }>((done) => {
      child.on("error", (failure) => { error = failure.message; });
      child.once("close", (exit, signal) => done({ exit, signal }));
    });
    clearTimeout(timer);
    return {
      name: options.name, executable: options.executable, argv: options.argv,
      ...completion, exit: error ? null : completion.exit, error, timedOut, stdout, stderr,
      durationMs: performance.now() - started,
    };
  } finally { closeSync(out); closeSync(err); }
}

/** Mandatory groups are separate: a successful workload cannot prove authority. */
export const mandatoryControls = [
  "workload.build", "workload.test", "workload.start", "workload.resolution",
  "network.allow", "network.deny", "prerequisites.reject",
  "filesystem.exact-file", "filesystem.subtree", "filesystem.escape",
  "filesystem.restoration", "environment.descendants", "handles.denied",
  "managed.completion", "managed.failure", "managed.ctrl-c",
  "limits.timeout", "limits.output", "limits.processes", "limits.memory",
  "compatibility.addons", "compatibility.bypass",
] as const;

export function validateEvidence(value: unknown, directory: string) {
  const errors: string[] = [];
  const report = value as any;
  if (report?.schemaVersion !== 1) errors.push("unsupported evidence schema");
  const text = (value: unknown) => typeof value === "string" && value.trim().length > 0;
  const digest = (value: unknown) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
  if (!/^[a-f0-9]{40}$/.test(report?.source?.commit ?? "") || report?.source?.clean !== true)
    errors.push("clean source commit required");
  for (const key of ["edition", "build", "architecture"])
    if (!text(report?.platform?.[key])) errors.push(`platform.${key} required`);
  if (report?.platform?.os !== "win32") errors.push("native Windows evidence required");
  for (const key of ["cli", "workloadNode", "compatibility", "guard"])
    if (!digest(report?.identities?.[key]?.sha256) || !text(report?.identities?.[key]?.description))
      errors.push(`${key} identity required`);
  for (const key of ["developmentNode", "rust", "officialNodeProvenance"])
    if (!text(report?.[key])) errors.push(`${key} required`);
  const attachments = new Map<string, Buffer>();
  for (const item of Array.isArray(report?.attachments) ? report.attachments : []) {
    try {
      if (!text(item.path) || isAbsolute(item.path)) throw new Error("relative path required");
      const root = realpathSync(directory), path = realpathSync(resolve(root, item.path));
      const local = relative(root, path);
      if (local === ".." || local.startsWith(`..${sep}`) || isAbsolute(local)) throw new Error("outside evidence directory");
      if (attachments.has(item.path)) throw new Error("duplicate attachment");
      const bytes = readFileSync(path);
      if (createHash("sha256").update(bytes).digest("hex") !== item.sha256) throw new Error("digest mismatch");
      attachments.set(item.path, bytes);
    } catch (error) { errors.push(`attachment ${item?.path}: ${String(error)}`); }
  }
  const controls = Array.isArray(report?.controls) ? report.controls : [];
  for (const id of mandatoryControls) {
    const matches = controls.filter((control: any) => control?.id === id);
    if (matches.length !== 1 || matches[0].status !== "pass" ||
        matches[0].commit !== report?.source?.commit ||
        matches[0].cliSha256 !== report?.identities?.cli?.sha256 ||
        !Array.isArray(matches[0].attachments) || matches[0].attachments.length === 0 ||
        matches[0].attachments.some((path: string) => !attachments.has(path)))
      errors.push(`${id}: passing source-bound attachments required`);
  }
  for (const name of ["install", "build", "test", "start"]) {
    const commands = Array.isArray(report?.commands) ? report.commands.filter((item: any) => item?.name === name) : [];
    const command = commands[0];
    if (commands.length !== 1 || (command.exit !== 0 && !(name === "start" && command.exit === 130)) || !Array.isArray(command.argv) ||
        !command.argv.every(text) || !command.argv.length ||
        command.error != null || command.timedOut === true || command.signal != null ||
        !Number.isFinite(command.durationMs) || command.durationMs < 0 ||
        !attachments.has(command.stdout) || !attachments.has(command.stderr)) {
      errors.push(`${name}: successful command and separate logs required`);
      continue;
    }
    if (command.stdout === command.stderr) errors.push(`${name}: streams must remain separate`);
    if (name !== "install") {
      try {
        const result = JSON.parse(attachments.get(command.receipt)!.toString("utf8"));
        if (result.schema_version !== 1 || result.operation !== "run" || result.outcome !== "completed" || result.error !== null || !result.receipt)
          throw new Error("invalid successful result");
        const receipt = result.receipt;
        if (receipt.schema_version !== 1 || !text(receipt.backend?.name) ||
            !["Restricted", "ManagedTree"].includes(receipt.assurance) ||
            receipt.termination !== (command.exit === 130 ? "Cancelled" : "Exited(0)") ||
            !Array.isArray(receipt.effective_filesystem) ||
            !Array.isArray(receipt.established_evidence)) throw new Error("malformed receipt");
        for (const key of ["requested", "declared", "observed", "enforced"])
          if (!receipt[key] || typeof receipt[key] !== "object" || Array.isArray(receipt[key]))
            throw new Error("missing authority dimensions");
        for (const dimension of [
          "filesystem_read", "filesystem_write", "network", "environment_sanitization",
          "descriptor_hygiene", "subprocess_restriction", "descendant_authority_propagation",
          "descendant_lifecycle", "process_tree_membership", "complete_cleanup",
          "timeout", "output", "process_count", "memory",
        ]) {
          for (const key of ["requested", "declared", "observed", "enforced"])
            if (typeof receipt[key][dimension] !== "boolean") throw new Error("invalid dimension");
          if (receipt.requested[dimension] !== receipt.enforced[dimension] ||
              (receipt.requested[dimension] && (!receipt.declared[dimension] || !receipt.observed[dimension])))
            throw new Error("unestablished requested authority");
        }
        if (receipt.assurance === "ManagedTree" &&
            (receipt.completion?.cleanup_confidence !== "KernelOwnedComplete" ||
             receipt.completion?.confirmed?.complete_cleanup !== true))
          throw new Error("ManagedTree cleanup not proven");
      } catch { errors.push(`${name}: trusted successful result required`); }
    }
  }
  if (report?.readiness?.body !== "TAPID_NEWS_SITE_ACCEPTANCE_V1" ||
      !Number.isFinite(report?.readiness?.durationMs) || report.readiness.durationMs < 0)
    errors.push("readiness response and duration required");
  if (report?.cleanup?.method !== "console-ctrl-c" || report?.cleanup?.confirmed !== true ||
      !Number.isFinite(report?.cleanup?.durationMs) || report.cleanup.durationMs < 0)
    errors.push("confirmed graceful console cleanup required");
  return { complete: errors.length === 0, errors };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const path = resolve(process.argv[2]);
    const result = validateEvidence(JSON.parse(readFileSync(path, "utf8")), resolve(path, ".."));
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result.complete ? 0 : 1;
  } catch (error) {
    console.error(String(error));
    process.exitCode = 1;
  }
}
