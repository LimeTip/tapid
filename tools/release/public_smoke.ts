import { createHash } from 'node:crypto';
import { validateReleaseIntent } from './candidate.ts';
import { PublicTransport, PUBLIC_LIMITS } from './public_transport.ts';

// These hashes pin the complete reviewed source-selection and resolver wiring.
// A workflow edit requires another review before its evidence can be accepted.
const REVIEWED = {
  'release-public-smoke.yml': 'f9d1103eb7c29726b9dc29ae7fa715889fe267c326b403906c9fbf528e937a9e',
  'crates-publication.yml': '4eb37a029e390ee4c86a5fd8d44c04036c99155f42ccdb473e497a558c8e8e8c',
};
const API = 'https://api.github.com/repos/LimeTip/tapid';
const REQUIRED = ['resolve', 'Unix installer (ubuntu-latest)', 'Unix installer (macos-latest)', 'Windows installer'];
export type SmokeEvidence = {
  releaseId: number;
  tag: string;
  commit: string;
  runId: number;
  attempt: number;
  contract: 'release-event' | 'coordinator-push';
  workflowCommit: string;
  workflowSha256: string;
  resolverStartedAt: string;
  jobs: { id: number; name: string }[];
};
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('malformed public smoke evidence');
  return value as Record<string, unknown>;
}
function positive(value: unknown): value is number { return typeof value === 'number' && Number.isSafeInteger(value) && value > 0; }
async function reviewed(transport: PublicTransport, commit: string, name: keyof typeof REVIEWED): Promise<void> {
  const bytes = await transport.bytes(`https://raw.githubusercontent.com/LimeTip/tapid/${commit}/.github/workflows/${name}`, PUBLIC_LIMITS.metadataBytes);
  if (createHash('sha256').update(bytes).digest('hex') !== REVIEWED[name]) throw Error('unreviewed public smoke workflow revision');
}

/** Accept only publicly provable selections. Recovery inputs are not public run data. */
export async function verifyPublicSmoke(transport: PublicTransport, identity: { tag: string; commit: string; releaseId: number; publishedAt: string }): Promise<SmokeEvidence> {
  if (!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(identity.publishedAt) || !Number.isFinite(Date.parse(identity.publishedAt))) throw Error('invalid public release publication time');
  const candidates: { run: Record<string, unknown>; workflow: keyof typeof REVIEWED }[] = [];
  for (const workflow of ['release-public-smoke.yml', 'crates-publication.yml'] as const) {
    const runs = await transport.list(`${API}/actions/workflows/${workflow}/runs?head_sha=${identity.commit}`, 'workflow_runs');
    for (const value of runs) {
      const run = object(value);
      if (!positive(run.id) || !positive(run.run_attempt) || typeof run.head_sha !== 'string' || !/^[a-f0-9]{40}$/.test(run.head_sha)) throw Error('malformed public smoke run identity');
      const [path, ref, ...extra] = typeof run.path === 'string' ? run.path.split('@') : [];
      if (path !== `.github/workflows/${workflow}` || extra.length || ref !== undefined && !/^[^\s]+$/.test(ref)) throw Error('inconsistent public smoke workflow identity');
      if (run.status === 'completed' && run.conclusion === 'success') candidates.push({ run, workflow });
    }
  }
  candidates.sort((a, b) => Number(b.run.id) - Number(a.run.id) || Number(b.run.run_attempt) - Number(a.run.run_attempt));
  const seen = new Set<number>();
  for (const { run, workflow } of candidates) {
    if (seen.has(Number(run.id))) throw Error('duplicate public smoke run');
    seen.add(Number(run.id));
    const coordinator = workflow === 'crates-publication.yml';
    // A workflow_dispatch recovery may select a tag unrelated to head_sha; no
    // anonymous API field proves those inputs. Do not guess from its title.
    if (run.head_sha !== identity.commit || (coordinator ? run.event !== 'push' || run.head_branch !== 'main' : run.event !== 'release' || run.head_branch !== identity.tag)) continue;
    try {
      await reviewed(transport, run.head_sha as string, workflow);
      if (coordinator) {
        await reviewed(transport, run.head_sha as string, 'release-public-smoke.yml');
        const intent = validateReleaseIntent(await transport.json(`https://raw.githubusercontent.com/LimeTip/tapid/${run.head_sha}/docs/releases/intent.json`));
        if (`v${intent.version}` !== identity.tag) continue;
      }
      const jobs = (await transport.list(`${API}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs`, 'jobs')).map(object);
      const jobIds = new Set<number>();
      let duplicate = false;
      for (const job of jobs) {
        if (!positive(job.id) || jobIds.has(job.id)) { duplicate = true; break; }
        jobIds.add(job.id);
      }
      if (duplicate) continue;
      const selected: SmokeEvidence['jobs'] = [];
      for (const name of REQUIRED) {
        const matching = jobs.filter(job => job.name === (coordinator ? `smoke / ${name}` : name));
        if (matching.length !== 1) break;
        const job = matching[0];
        if (job.run_id !== run.id || (job.run_attempt !== undefined && job.run_attempt !== run.run_attempt) || job.head_sha !== run.head_sha || job.status !== 'completed' || job.conclusion !== 'success') break;
        // The current immutable release must already exist when the resolver
        // reads it. This excludes smoke for an older, deleted release sharing
        // the same tag and source commit.
        if (typeof job.started_at !== 'string' || !Number.isFinite(Date.parse(job.started_at)) || Date.parse(job.started_at) < Date.parse(identity.publishedAt)) break;
        selected.push({ id: Number(job.id), name: String(job.name) });
      }
      if (selected.length !== REQUIRED.length) continue;
      return {
        releaseId: identity.releaseId, tag: identity.tag, commit: identity.commit,
        runId: Number(run.id), attempt: Number(run.run_attempt), contract: coordinator ? 'coordinator-push' : 'release-event',
        workflowCommit: run.head_sha as string, workflowSha256: REVIEWED[workflow], jobs: selected,
        resolverStartedAt: String(jobs.find(job => job.id === selected[0].id)!.started_at),
      };
    } catch {
      // An unusable candidate cannot hide a later qualifying run. Transport's
      // shared deadline/transfer budget still bounds the entire search.
      continue;
    }
  }
  throw Error('no successful public smoke attempt with independently provable tag and commit selection');
}
