/** Artifact-only reporting. No credentials, public reads, or publication operations. */
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { aggregateSmoke } from './public_smoke.ts';

const [mode, directory] = process.argv.slice(2);
if (!directory || !['platform', 'aggregate'].includes(mode)) throw Error('usage: public_smoke_report.ts platform|aggregate DIRECTORY');
if (mode === 'platform') {
  let identity: unknown = null;
  try { identity = JSON.parse(await readFile(join(directory, 'identity.json'), 'utf8')); } catch { /* Missing identity remains a failing report. */ }
  const steps = JSON.parse(process.env.SMOKE_STEPS || '{}');
  const checks = Object.fromEntries(['install_published', 'selected_consumer', 'install_public', 'install_latest', 'latest_consumer', 'upgrade', 'previous_repeat']
    .map(name => [name, steps[name]?.outcome || (name === 'upgrade' && process.env.SMOKE_PLATFORM === 'Windows' ? steps.previous_repeat?.outcome : undefined) || 'missing']));
  let skips: string[] = [];
  try { skips = (await readFile(join(directory, '..', 'public-upgrade.txt'), 'utf8')).split('\n').filter(line => line.startsWith('Skip ')); } catch { /* No attempted upgrade yet. */ }
  await writeFile(join(directory, 'platform.json'), JSON.stringify({ schema: 'tapid-public-smoke-platform-v1', identity, platform: process.env.SMOKE_PLATFORM, checks, skips }, null, 2));
} else {
  const reports: unknown[] = []; let inventory: unknown = null;
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    for (const name of ['inventory.json', 'platform.json']) {
      try {
        const value = JSON.parse(await readFile(join(directory, entry.name, name), 'utf8'));
        if (name === 'inventory.json') {
          if (inventory) throw Error('duplicate inventory');
          inventory = value;
        } else reports.push(value);
      } catch (error: any) { if (error.code !== 'ENOENT') reports.push({ malformed: true }); }
    }
  }
  const needs = JSON.parse(process.env.SMOKE_JOBS || '{}');
  const jobs = Object.fromEntries(Object.entries(needs).map(([name, value]: [string, any]) => [name, value.result]));
  const result = aggregateSmoke(inventory, reports, jobs, `${process.env.GITHUB_RUN_ID}/${process.env.GITHUB_RUN_ATTEMPT}`, process.env.EVENT_NAME === 'schedule');
  await writeFile(join(directory, 'summary.json'), JSON.stringify(result, null, 2));
  const markdown = [`# Public release smoke: ${result.status}`, '', `Coverage: ${result.coverage}`, '',
    `Selected: ${result.identity?.selected.tag ?? 'unavailable'} (${result.identity?.selected.commit ?? 'unavailable'})`,
    `Latest: ${result.identity?.latest.tag ?? 'unavailable'} (${result.identity?.latest.commit ?? 'unavailable'})`, '',
    ...result.assets.map((asset: any) => `- ${asset.tag}/${asset.name}: ${asset.size} bytes, SHA-256 ${asset.sha256}; ${asset.url}`), '',
    ...result.platforms.map((report: any) => `- ${report?.platform ?? 'Malformed platform'}: ${JSON.stringify(report?.checks ?? {})}`), '',
    ...result.publicUrls.map((url: string) => `- Public URL: ${url}`), '',
    ...[...result.skips, ...result.platforms.flatMap((report: any) => report?.skips ?? [])].map((skip: string) => `- Skip: ${skip}`), '',
    ...result.failures.map(failure => `- FAILED: ${failure}`), ''].join('\n');
  await writeFile(join(directory, 'summary.md'), markdown);
  if (process.env.GITHUB_STEP_SUMMARY) await writeFile(process.env.GITHUB_STEP_SUMMARY, markdown);
  if (result.status !== 'passed') process.exitCode = 1;
}
