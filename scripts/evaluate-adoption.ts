// Opt-in evidence capture. This module supplies neither sandboxing nor hook authority.
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { digest } from './check-doc-examples.ts';

type Phase = {
  id: string; executable: string; args: string[]; cwd: string;
  env: Record<string, string>; timeoutMs: number; outputLimit: number;
  prerequisites?: string[]; contains?: string[]; inputs?: string[]; outputs?: string[];
  manualSteps?: string[]; unsupported?: string;
};
type Specification = {
  version: 1; id: string; project: string; destination: string;
  secrets?: string[]; phases: Phase[];
};
type Receipt = {
  id: string; outcome: 'passed' | 'failed' | 'unsupported' | 'not-reached';
  command: string[]; cwd: string; started: string; elapsedMs: number;
  exit: number | null; signal: string | null; reason: string | null;
  output: string; truncated: boolean; assertions: boolean[];
  inputs: Record<string, string>; outputs: Record<string, string>;
  manualSteps: string[];
};

export async function evaluate(spec: Specification) {
  if (spec.version !== 1) throw new Error('Unsupported specification version');
  const seen = new Set<string>();
  for (const p of spec.phases) {
    if (!p.env || typeof p.env !== "object" || Array.isArray(p.env) ||
        Object.values(p.env).some(value => typeof value !== "string"))
      throw new Error("An explicit environment of string values is required");
    if (!p.id || seen.has(p.id) || p.prerequisites?.some(id => !seen.has(id)))
      throw new Error('Phase IDs must be unique and prerequisites must precede dependents');
    if (!Number.isSafeInteger(p.timeoutMs) || p.timeoutMs <= 0 ||
        !Number.isSafeInteger(p.outputLimit) || p.outputLimit <= 0)
      throw new Error('Positive integer deadline and output limit required');
    seen.add(p.id);
  }
  const clean = (value: string) => {
    for (const secret of [...(spec.secrets ?? [])].filter(Boolean).sort((a,b) => b.length-a.length))
      value = value.split(secret).join('[REDACTED]');
    return value.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g,
      c => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`);
  };
  const report = { version: 1, id: spec.id, project: spec.project,
    counting: 'Runner launches including failed launches; internal subprocesses excluded',
    cleanup: process.platform === 'win32' ? 'Root only; unattended external evaluation unsupported' : 'Process group; detached descendants are outside this guarantee',
    launches: 0, phases: [] as Receipt[] };
  mkdirSync(spec.destination, { recursive: true });
  const publish = () => {
    // Sanitize every string and key, including paths and launch errors.
    const sanitize = (value: unknown): unknown => {
      if (typeof value === "string") return clean(value);
      if (Array.isArray(value)) return value.map(sanitize);
      if (value && typeof value === "object") return Object.fromEntries(
        Object.entries(value).map(([key, item]) => [clean(key), sanitize(item)]));
      return value;
    };
    const publicReport = sanitize(report) as typeof report;
    writeFileSync(join(spec.destination, 'receipts.json'), JSON.stringify(publicReport, null, 2) + '\n');
    writeFileSync(join(spec.destination, 'report.md'), '# Adoption capture\n\n' +
      publicReport.phases.map((p: Receipt) => `- ${JSON.stringify(p.id)}: ${p.outcome}; ${p.reason ?? 'assertions satisfied'}`).join('\n') + '\n');
    return publicReport;
  };
  for (const p of spec.phases) {
    const start = Date.now();
    const r: Receipt = { id: p.id, outcome: 'failed', command: [p.executable, ...p.args], cwd: p.cwd,
      started: new Date(start).toISOString(), elapsedMs: 0, exit: null, signal: null,
      reason: null, output: '', truncated: false, assertions: [], inputs: {}, outputs: {}, manualSteps: p.manualSteps ?? [] };
    const blocked = p.prerequisites?.filter(id => report.phases.find(r => r.id === id)?.outcome !== 'passed');
    if (blocked?.length) { r.outcome = 'not-reached'; r.reason = `Prerequisites: ${blocked.join(', ')}`; }
    else if (p.unsupported) { r.outcome = 'unsupported'; r.reason = p.unsupported; }
    else {
      try {
        for (const path of p.inputs ?? []) r.inputs[path] = digest(path);
        report.launches++;
        await new Promise<void>(resolve => {
          const child = spawn(p.executable, p.args, { cwd: p.cwd, env: p.env,
            detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'] });
          let size = 0;
          const chunks: Buffer[] = [];
          const kill = () => {
            try { if (child.pid) {
              if (process.platform === 'win32') child.kill('SIGKILL');
              else process.kill(-child.pid, 'SIGKILL');
            } } catch (e) { if ((e as NodeJS.ErrnoException).code !== 'ESRCH') r.reason ??= String(e); }
            child.stdout.destroy(); child.stderr.destroy();
          };
          const timer = setTimeout(() => { r.reason ??= 'timeout'; kill(); }, p.timeoutMs);
          const collect = (chunk: Buffer) => {
            chunks.push(chunk.subarray(0, Math.max(0, p.outputLimit-size)));
            size += chunk.length;
            if (size > p.outputLimit) { r.truncated = true; r.reason ??= 'output-limit'; kill(); }
          };
          child.stdout.on('data', collect); child.stderr.on('data', collect);
          child.once('error', e => { r.reason = `launch: ${e.message}`; });
          child.once('close', (code, signal) => {
            clearTimeout(timer); kill(); r.exit = code; r.signal = signal;
            // Hide any sensitive suffix cut by the raw byte limit before public export.
            let output = Buffer.concat(chunks).toString('utf8');
            r.assertions = (p.contains ?? []).map(text => output.includes(text));
            if (r.truncated) for (const secret of spec.secrets ?? []) {
              for (let n = secret.length - 1; n > 0; n--) if (output.endsWith(secret.slice(0,n))) {
                output = output.slice(0,-n) + '[REDACTED]'; break;
              }
            }
            r.output = clean(output); resolve();
          });
        });
        for (const path of p.outputs ?? []) r.outputs[path] = digest(path);
        if (!r.reason && r.exit === 0 && r.assertions.every(Boolean)) r.outcome = 'passed';
        else r.reason ??= r.exit !== 0 ? 'nonzero-exit' : 'assertion';
      } catch (e) { r.reason = String(e); }
    }
    r.elapsedMs = Date.now()-start; report.phases.push(r); publish();
  }
  return publish() as typeof report;
}
