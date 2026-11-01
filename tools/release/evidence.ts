import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile, mkdir, appendFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { candidateFingerprint, validateUnsignedCandidate, validateCandidate, validateReleaseIntent, type UnsignedCandidate } from './candidate.ts';
import { isPublished, type Package } from './publish.ts';
import { githubAdapter } from './candidate_github.ts';

export type RunContext = { repository: string; run_id: number; attempt: number; workflow_sha: string };
export type Authorization = RunContext & {
  schema: 'tapid-release-authorization-v1'; source: string; unsigned_sha256: string;
  actor: string; actor_id: number; environment: 'stable-release'; environment_id: number;
  job_id: number; job_url: string; review_url: string;
  approved_attempt: 1; reused_authorization_artifact_id: number | null;
};
type Command = (args: string[]) => Promise<string>;
const gh: Command = args => new Promise((resolve, reject) => {
  execFile('gh', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 }, (error, stdout) => {
    // API errors may include response content; keep it out of retained evidence.
    if (error) reject(Error('GitHub evidence request failed')); else resolve(stdout);
  });
});
function positive(value: number): void {
  if (!Number.isSafeInteger(value) || value <= 0) throw Error('invalid evidence ID');
}
function validateContext(context: RunContext): void {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(context.repository)) throw Error('invalid repository');
  positive(context.run_id); positive(context.attempt);
  if (!/^[a-f0-9]{40}$/.test(context.workflow_sha)) throw Error('invalid workflow source');
}
export async function collectAuthorization(context: RunContext, input: UnsignedCandidate, command: Command = gh, retained?: { authorization: Authorization; artifact_id: number }): Promise<Authorization> {
  validateContext(context);
  const candidate = validateUnsignedCandidate(input);
  if (candidate.repository !== context.repository) throw Error('authorization repository mismatch');
  // Review history has neither a review time nor an attempt ID. Reruns need the
  // immutable first-attempt receipt downloaded from this run, plus identical bytes.
  if (context.attempt !== 1) {
    if (!retained) throw Error('authorization attempt cannot be proven; dispatch a new recovery run');
    positive(retained.artifact_id);
    const prior = retained.authorization;
    if (prior.schema !== 'tapid-release-authorization-v1' || prior.attempt !== 1 || prior.approved_attempt !== 1 || prior.repository !== context.repository || prior.run_id !== context.run_id || prior.workflow_sha !== context.workflow_sha || prior.source !== candidate.commit || prior.unsigned_sha256 !== candidateFingerprint(candidate)) throw Error('retained authorization mismatch');
  }
  const base = `repos/${context.repository}/actions/runs/${context.run_id}`;
  const request = async (path: string, paginated = false) => JSON.parse(await command(['api', path, ...(paginated ? ['--paginate', '--slurp'] : [])]));
  const run = await request(`${base}/attempts/${context.attempt}`);
  if (run.id !== context.run_id || run.run_attempt !== context.attempt || run.head_sha !== context.workflow_sha || run.head_branch !== 'main' ||
      run.path !== '.github/workflows/crates-publication.yml' || !['push', 'workflow_dispatch'].includes(run.event)) throw Error('authorization run identity mismatch');
  const pages = await request(`${base}/approvals`, true);
  if (!Array.isArray(pages) || pages.some(page => !Array.isArray(page)) || pages.flat().some(review => !review || !['approved', 'rejected'].includes(review.state) || !Array.isArray(review.environments) || review.environments.some((env: {name: string}) => !env || typeof env.name !== 'string'))) throw Error('malformed review history');
  const reviews = pages.flat().filter(review => Array.isArray(review.environments) && review.environments.some((env: {name: string}) => env.name === 'stable-release'));
  if (reviews.length !== 1 || reviews[0].state !== 'approved') throw Error('missing, rejected or ambiguous candidate authorization');
  const review = reviews[0];
  if (!review.user || review.user.type !== 'User' || !/^[A-Za-z0-9-]+$/.test(review.user.login) ||
      [run.actor?.login, run.triggering_actor?.login].some(login => !login || login.toLowerCase() === review.user.login.toLowerCase())) throw Error('authorization must have an independent reviewer');
  positive(review.user.id);
  if (context.attempt !== 1 && (review.user.id !== retained!.authorization.actor_id || review.user.login !== retained!.authorization.actor)) throw Error('retained authorization reviewer mismatch');
  const environments = review.environments.filter((env: {name: string}) => env.name === 'stable-release');
  if (environments.length !== 1) throw Error('ambiguous approval environment');
  positive(environments[0].id);
  if (context.attempt !== 1 && environments[0].id !== retained!.authorization.environment_id) throw Error('retained authorization environment mismatch');
  const jobPages = await request(`${base}/attempts/${context.attempt}/jobs?per_page=100`, true);
  if (!Array.isArray(jobPages) || jobPages.some(page => !Array.isArray(page.jobs))) throw Error('malformed job history');
  const jobs = jobPages.flatMap(page => page.jobs).filter(item => item.name === 'candidate / draft-release');
  // The attempt-scoped endpoint supplies provenance; the documented job object
  // does not require run_attempt. Reject it if supplied inconsistently.
  if (jobs.length !== 1 || jobs[0].run_id !== context.run_id || (jobs[0].run_attempt !== undefined && jobs[0].run_attempt !== context.attempt) || jobs[0].head_sha !== context.workflow_sha || jobs[0].status !== 'in_progress') throw Error('protected job identity mismatch');
  positive(jobs[0].id);
  return {
    ...context, schema: 'tapid-release-authorization-v1', source: candidate.commit, unsigned_sha256: candidateFingerprint(candidate),
    actor: review.user.login, actor_id: review.user.id, environment: 'stable-release', environment_id: environments[0].id,
    job_id: jobs[0].id, job_url: `https://github.com/${context.repository}/actions/runs/${context.run_id}/job/${jobs[0].id}`,
    review_url: `https://api.github.com/${base}/approvals`,
    approved_attempt: 1, reused_authorization_artifact_id: context.attempt === 1 ? null : retained!.artifact_id,
  };
}

const stages = ['preflight', 'select', 'candidate', 'verify', 'promote', 'smoke', 'publish'] as const;
const platformJobs = [
  ...['ubuntu-latest', 'ubuntu-24.04-arm', 'macos-15-intel', 'macos-14', 'windows-latest', 'windows-11-vs2026-arm'].map(runner => `verify / Install and execute (${runner})`),
  'smoke / Unix installer (ubuntu-latest)', 'smoke / Unix installer (macos-latest)', 'smoke / Windows installer',
];
type JobRecord = { id: number; run_id: number; run_attempt?: number; head_sha: string; name: string; conclusion: string | null };
type EvidenceFiles = {
  unsigned?: unknown; signed?: unknown; authorization?: unknown; intent?: string; clean_install?: unknown;
  registry?: { confirmed: Package[]; remaining: Package[] };
  registry_observations?: (Package & { state: 'published' | 'missing' | 'unavailable' })[];
  preparation_pr?: number;
  artifacts?: { id: number; name: string; digest: string; size_in_bytes: number }[];
  public_readback?: { tag: { object: string; commit: string } | null; release: { id: number; tag_name: string; name: string; body: string; draft: boolean; prerelease: boolean; immutable?: boolean; assets: { id: number; name: string; size: number; digest?: string }[] } };
};

/** Reporting is read-only: a ledger is never accepted as publication authority. */
export function completionLedger(context: RunContext, needs: Record<string, { result: string }>, jobs: JobRecord[], files: EvidenceFiles) {
  validateContext(context);
  const run_url = `https://github.com/${context.repository}/actions/runs/${context.run_id}/attempts/${context.attempt}`;
  const missing: string[] = [];
  const results = stages.map(stage => {
    const result = needs[stage]?.result;
    if (result !== 'success') missing.push(`${stage}: ${['failure', 'skipped', 'cancelled'].includes(result) ? result : 'unavailable'}`);
    const matches = jobs.filter(job => job.name === stage && job.run_id === context.run_id && (job.run_attempt === undefined || job.run_attempt === context.attempt) && job.head_sha === context.workflow_sha);
    const jobId = matches.length === 1 ? matches[0].id : null;
    if (jobId) positive(jobId);
    return { stage, result: ['success', 'failure', 'skipped', 'cancelled'].includes(result) ? result : 'unavailable', job_id: jobId, url: jobId ? `https://github.com/${context.repository}/actions/runs/${context.run_id}/job/${jobId}` : run_url };
  });
  const platforms = platformJobs.map(name => {
    const matches = jobs.filter(job => job.name === name && job.run_id === context.run_id && (job.run_attempt === undefined || job.run_attempt === context.attempt) && job.head_sha === context.workflow_sha);
    const job = matches.length === 1 ? matches[0] : null;
    if (job) positive(job.id);
    const result = job?.conclusion && ['success', 'failure', 'cancelled', 'skipped', 'timed_out', 'neutral', 'action_required'].includes(job.conclusion) ? job.conclusion : 'unavailable';
    if (result !== 'success') missing.push(`${name}: unsuccessful or unavailable`);
    return { name, result, job_id: job?.id ?? null, url: job ? `https://github.com/${context.repository}/actions/runs/${context.run_id}/job/${job.id}` : run_url };
  });
  const unsigned = files.unsigned ? validateUnsignedCandidate(files.unsigned) : null;
  const signed = files.signed ? validateCandidate(files.signed) : null;
  if (!unsigned) missing.push('unsigned candidate unavailable');
  if (!signed) missing.push('signed candidate unavailable');
  if (unsigned && unsigned.repository !== context.repository) throw Error('ledger repository mismatch');
  if (signed) {
    const { schema: _schema, release_id: _id, assets, ...identity } = signed;
    const derived = { ...identity, schema: 'tapid-release-unsigned-candidate-v1', assets: assets.filter(asset => !asset.name.endsWith('.sig')).map(({ id: _assetId, ...asset }) => asset) } as UnsignedCandidate;
    if (!unsigned || candidateFingerprint(derived) !== candidateFingerprint(unsigned)) throw Error('signed candidate does not match authorized unsigned identity');
  }
  let authorization: Authorization | null = null;
  if (files.authorization && unsigned) {
    const input = files.authorization as Authorization;
    // Retain only the documented allowlist. Raw API comments and payloads never enter the ledger.
    if (input.schema !== 'tapid-release-authorization-v1' || input.repository !== context.repository || input.run_id !== context.run_id || input.attempt !== context.attempt || input.workflow_sha !== context.workflow_sha ||
        input.source !== unsigned.commit || input.unsigned_sha256 !== candidateFingerprint(unsigned) || input.environment !== 'stable-release' || input.approved_attempt !== 1 || !/^[A-Za-z0-9-]+$/.test(input.actor)) throw Error('authorization candidate/run mismatch');
    if (context.attempt !== 1) positive(input.reused_authorization_artifact_id!);
    positive(input.actor_id); positive(input.environment_id); positive(input.job_id);
    const expectedJob = `https://github.com/${context.repository}/actions/runs/${context.run_id}/job/${input.job_id}`;
    const expectedReview = `https://api.github.com/repos/${context.repository}/actions/runs/${context.run_id}/approvals`;
    if (input.job_url !== expectedJob || input.review_url !== expectedReview) throw Error('invalid authorization reference');
    authorization = { ...context, schema: input.schema, source: input.source, unsigned_sha256: input.unsigned_sha256, actor: input.actor, actor_id: input.actor_id,
      environment: input.environment, environment_id: input.environment_id, job_id: input.job_id, job_url: expectedJob, review_url: expectedReview,
      approved_attempt: 1, reused_authorization_artifact_id: input.reused_authorization_artifact_id };
  } else missing.push('authorization unavailable');
  const intent = files.intent ? validateReleaseIntent(JSON.parse(files.intent)) : null;
  if (intent && unsigned) {
    if (createHash('sha256').update(files.intent!).digest('hex') !== unsigned.plan_sha256 || `v${intent.version}` !== unsigned.tag) throw Error('ledger package plan mismatch');
  } else missing.push('approved package plan unavailable');
  let registry: { confirmed: Package[]; remaining: Package[] } | null = null;
  if (intent && files.registry) {
    const approved = intent.packages;
    const progress = files.registry;
    if (JSON.stringify([...progress.confirmed, ...progress.remaining]) !== JSON.stringify(approved)) throw Error('registry progress must be an exact approved prefix and remaining suffix');
    registry = { confirmed: progress.confirmed.map(pkg => ({ name: pkg.name, version: pkg.version })), remaining: progress.remaining.map(pkg => ({ name: pkg.name, version: pkg.version })) };
    if (registry.remaining.length) missing.push('registry publication incomplete');
  } else missing.push('registry read-back unavailable');
  const registry_observations = (intent?.packages ?? []).map(pkg => {
    const matches = (files.registry_observations ?? []).filter(observation => observation.name === pkg.name && observation.version === pkg.version);
    const state = matches.length === 1 && ['published', 'missing', 'unavailable'].includes(matches[0].state) ? matches[0].state : 'unavailable';
    return { name: pkg.name, version: pkg.version, state };
  });
  const clean = files.clean_install as { version?: string; help?: boolean } | undefined;
  const clean_install = intent && clean?.version === `tapid ${intent.version}` && clean?.help === true ? { version: clean.version, help: true } : null;
  if (!clean_install) missing.push('clean locked install/version/help unavailable');
  const preparation_pr = files.preparation_pr ?? null;
  if (preparation_pr) positive(preparation_pr); else missing.push('preparation PR unavailable');
  const artifacts = (files.artifacts ?? []).map(artifact => {
    positive(artifact.id); positive(artifact.size_in_bytes);
    if (!/^(?:(?:un)?signed-release-candidate|release-authorization|release-install)-[1-9][0-9]*$/.test(artifact.name) || !/^sha256:[a-f0-9]{64}$/.test(artifact.digest)) throw Error('invalid evidence artifact identity');
    return { id: artifact.id, name: artifact.name, digest: artifact.digest, size_in_bytes: artifact.size_in_bytes, url: `https://github.com/${context.repository}/actions/runs/${context.run_id}/artifacts/${artifact.id}` };
  });
  if (!artifacts.length) missing.push('artifact identities unavailable');
  let public_readback: { release_id: number; url: string; immutable: true } | null = null;
  if (files.public_readback && signed) {
    const { tag, release } = files.public_readback;
    if (!tag || tag.object !== signed.tag_object || tag.commit !== signed.commit || release.id !== signed.release_id || release.tag_name !== signed.tag || release.name !== signed.title ||
        createHash('sha256').update(release.body).digest('hex') !== signed.notes_sha256 || release.assets.length !== signed.assets.length ||
        signed.assets.some(asset => !release.assets.some(actual => actual.id === asset.id && actual.name === asset.name && actual.size === asset.size && actual.digest === `sha256:${asset.sha256}`))) throw Error('final public candidate read-back mismatch');
    if (release.draft || release.prerelease || release.immutable !== true) missing.push('final public release not yet public and immutable');
    else public_readback = { release_id: signed.release_id, url: `https://github.com/${context.repository}/releases/tag/${signed.tag}`, immutable: true };
  } else missing.push('final public read-back unavailable');
  return {
    schema: 'tapid-release-evidence-v1' as const, ...context, run_url,
    state: missing.length ? 'incomplete' as const : 'complete' as const,
    candidate: unsigned, signed_candidate: signed,
    unsigned_sha256: unsigned ? candidateFingerprint(unsigned) : null, signed_sha256: signed ? candidateFingerprint(signed) : null,
    authorization, approved_packages: intent?.packages ?? null, registry, registry_observations, clean_install,
    preparation: intent ? { prepared_from: intent.prepared_from, source: unsigned?.commit ?? null, pr: preparation_pr, url: preparation_pr ? `https://github.com/${context.repository}/pull/${preparation_pr}` : null } : null,
    artifacts, public_readback,
    stages: results, platforms, missing,
    recovery_url: `https://github.com/${context.repository}/actions/workflows/crates-publication.yml`,
  };
}
export function renderLedger(ledger: ReturnType<typeof completionLedger>): string {
  const text = (value: string) => value.replace(/[\r\n]/g, ' ').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/[\\`*_{}[\]()!#|]/g, '\\$&');
  return `### Release ${ledger.state === 'complete' ? 'complete' : 'incomplete'}\n\n[Exact run and job logs](${ledger.run_url}) · [Recover through the coordinator](${ledger.recovery_url})\n\n${ledger.stages.map(stage => `- [${stage.stage}: ${stage.result}](${stage.url})`).join('\n')}\n\nMissing evidence: ${ledger.missing.length}. Download the release-evidence artifact for candidate identities, platform results and registry progress. Failed or skipped stages do not authorize publication.\n\n${ledger.missing.map(reason => `- ${text(reason)}`).join('\n')}\n`;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [action, input, output] = process.argv.slice(2);
  if (!['authorize', 'report'].includes(action) || !input || !output) throw Error('usage: evidence.ts authorize unsigned.json authorization.json | report artifacts output-directory');
  const context = {
    repository: process.env.GITHUB_REPOSITORY!, run_id: Number(process.env.GITHUB_RUN_ID),
    attempt: Number(process.env.GITHUB_RUN_ATTEMPT), workflow_sha: process.env.GITHUB_SHA!,
  };
  if (action === 'authorize') {
    const retainedId = process.env.RETAINED_AUTHORIZATION_ARTIFACT_ID;
    const retained = retainedId ? { artifact_id: Number(retainedId), authorization: JSON.parse(await readFile('prior-authorization/authorization.json', 'utf8')) } : undefined;
    const authorization = await collectAuthorization(context, JSON.parse(await readFile(input, 'utf8')), gh, retained);
    await writeFile(output, `${JSON.stringify(authorization, null, 2)}\n`, { flag: 'wx' });
  } else {
    validateContext(context);
    const unavailable: string[] = [];
    if (process.env.EVIDENCE_DOWNLOAD_OUTCOME !== 'success') unavailable.push('upstream evidence download unsuccessful or unavailable');
    const optional = async (path: string, json = true) => {
      try { const text = await readFile(path, 'utf8'); return json ? JSON.parse(text) : text; }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
        throw error;
      }
    };
    const attempt = context.attempt;
    const signedDir = join(input, `signed-release-candidate-${attempt}`);
    const unsignedDir = join(input, `unsigned-release-candidate-${attempt}`);
    const files: EvidenceFiles = {};
    let jobs: JobRecord[] = [];
    let ledger: ReturnType<typeof completionLedger>;
    try {
      files.unsigned = await optional(join(unsignedDir, 'unsigned.json')) ?? await optional(join(signedDir, 'unsigned.json'));
      files.signed = await optional(join(signedDir, 'signed.json'));
      files.intent = await optional(join(unsignedDir, 'intent.json'), false) ?? await optional(join(signedDir, 'intent.json'), false);
      files.authorization = await optional(join(input, `release-authorization-${attempt}`, 'authorization.json'));
      files.clean_install = await optional(join(input, `release-install-${attempt}`, 'clean-install.json'));
      try {
        const pages = JSON.parse(await gh(['api', `repos/${context.repository}/actions/runs/${context.run_id}/attempts/${attempt}/jobs?per_page=100`, '--paginate', '--slurp']));
        if (!Array.isArray(pages) || pages.some(page => !Array.isArray(page.jobs))) throw Error('invalid jobs');
        jobs = pages.flatMap(page => page.jobs);
      } catch { unavailable.push('attempt-specific job read-back unavailable'); }
      if (files.intent) {
        const intent = validateReleaseIntent(JSON.parse(files.intent));
        const confirmed: Package[] = [], remaining: Package[] = [];
        files.registry_observations = [];
        for (const pkg of intent.packages) {
          try {
            const published = await isPublished(pkg);
            (published ? confirmed : remaining).push(pkg);
            files.registry_observations.push({ ...pkg, state: published ? 'published' : 'missing' });
          } catch { files.registry_observations.push({ ...pkg, state: 'unavailable' }); }
        }
        if (files.registry_observations.every(observation => observation.state !== 'unavailable')) files.registry = { confirmed, remaining };
        else unavailable.push('independent registry read-back failed for some versions');
      }
      try {
        const pages = JSON.parse(await gh(['api', `repos/${context.repository}/actions/runs/${context.run_id}/artifacts?per_page=100`, '--paginate', '--slurp']));
        files.artifacts = pages.flatMap((page: {artifacts: EvidenceFiles['artifacts']}) => page.artifacts ?? []).filter((artifact: {name: string}) => /^(?:(?:un)?signed-release-candidate|release-authorization|release-install)-[1-9][0-9]*$/.test(artifact.name));
      } catch { unavailable.push('artifact identity read-back unavailable'); }
      if (files.unsigned) {
        const candidate = validateUnsignedCandidate(files.unsigned);
        try {
          const pages = JSON.parse(await gh(['api', `repos/${context.repository}/commits/${candidate.commit}/pulls?per_page=100`, '--paginate', '--slurp']));
          const prs = pages.flat().filter((pr: {merged_at: string | null; merge_commit_sha: string; base: {ref: string}}) => pr.merged_at && pr.merge_commit_sha === candidate.commit && pr.base.ref === 'main');
          if (prs.length === 1) files.preparation_pr = prs[0].number;
        } catch { unavailable.push('preparation PR read-back unavailable'); }
      }
      if (files.signed) {
        const candidate = validateCandidate(files.signed);
        try {
          const api = githubAdapter(context.repository);
          const release = JSON.parse(await gh(['api', `repos/${context.repository}/releases/${candidate.release_id}`]));
          files.public_readback = { tag: await api.tag(candidate.tag), release };
        } catch { unavailable.push('final public release read-back failed'); }
      }
      ledger = completionLedger(context, JSON.parse(process.env.RELEASE_NEEDS ?? '{}'), jobs, files);
    } catch {
      // Preserve a report even for malformed/mismatched inputs, without retaining
      // arbitrary error strings that could contain credential-bearing payloads.
      ledger = completionLedger(context, JSON.parse(process.env.RELEASE_NEEDS ?? '{}'), jobs, {});
      unavailable.push('artifact validation failed; inspect upstream evidence and job logs');
    }
    ledger.missing.push(...unavailable);
    if (ledger.missing.length) ledger.state = 'incomplete';
    await mkdir(output, { recursive: true });
    await writeFile(join(output, 'release-evidence.json'), `${JSON.stringify(ledger, null, 2)}\n`);
    const summary = renderLedger(ledger);
    await writeFile(join(output, 'summary.md'), summary);
    if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
  }
}
