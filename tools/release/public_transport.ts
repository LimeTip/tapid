/** Anonymous public reads only. This module never consults ambient credentials. */
export const PUBLIC_LIMITS = Object.freeze({
  apiBytes: 2 * 1024 * 1024,
  metadataBytes: 256 * 1024,
  archiveBytes: 512 * 1024 * 1024,
  aggregateBytes: 4 * 1024 * 1024 * 1024,
  pages: 10,
  attempts: 3,
  redirects: 5,
  retryDelayMs: 30_000,
  attemptMs: 60_000,
  overallMs: 20 * 60_000,
  sourceTransferBytes: 256 * 1024 * 1024,
  sourceMaterializedBytes: 1024 * 1024 * 1024,
  sourceEntries: 100_000,
});

type Options = {
  fetch?: typeof fetch;
  delay?: (ms: number) => Promise<void>;
  now?: () => number;
  transferBudgetBytes?: number;
  sourceTransferBudgetBytes?: number;
};

// Keep endpoint shapes explicit: a response cannot turn this into an arbitrary URL reader.
function publicUrl(input: string): URL {
  let url: URL;
  try { url = new URL(input); } catch { throw Error('invalid public URL'); }
  if (url.protocol !== 'https:' || url.username || url.password || url.port || url.hash || /[\\\s]/.test(input)) {
    throw Error('disallowed public URL');
  }
  const api = url.hostname === 'api.github.com' && /^\/repos\/LimeTip\/tapid\/(?:git\/ref\/tags\/v[0-9.]+|git\/tags\/[a-f0-9]{40}|releases|actions\/workflows\/(?:release-public-smoke|crates-publication)\.yml\/runs|actions\/runs\/[1-9][0-9]*\/attempts\/[1-9][0-9]*\/jobs)$/.test(url.pathname);
  // Query parameters belong only to bounded discovery endpoints.
  const query = [...url.searchParams];
  if (url.hostname === 'api.github.com' && /^\/repos\/LimeTip\/tapid\/git\/trees\/[a-f0-9]{40}$/.test(url.pathname)
    && url.search === '?recursive=1') return url;
  if (url.hostname === 'crates.io' && !url.search && /^\/api\/v1\/crates\/[A-Za-z0-9][A-Za-z0-9_-]*\/[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?$/.test(url.pathname)) return url;
  if (api && query.every(([key, value]) =>
    (key === 'per_page' && value === '100') || (key === 'page' && /^(?:[1-9]|10)$/.test(value)))) return url;
  if (url.hostname === 'tapid.dev' && !url.search && /^\/releases\/v1\/v[0-9]+\.[0-9]+\.[0-9]+\.tsv(?:\.sig)?$/.test(url.pathname)) return url;
  if (url.hostname === 'raw.githubusercontent.com' && !url.search
    && /^\/LimeTip\/tapid\/[a-f0-9]{40}\/(?:[A-Za-z0-9_.-]+\/)*(?:Cargo\.toml|Cargo\.lock|[A-Za-z0-9_.-]+\.rs)$/.test(url.pathname)
    && !url.pathname.split('/').some(part => part === '.' || part === '..')) return url;
  if (url.hostname === 'raw.githubusercontent.com' && !url.search && /^\/LimeTip\/tapid\/[a-f0-9]{40}\/(?:\.github\/workflows\/(?:release-public-smoke|crates-publication)\.yml|docs\/releases\/intent\.json|scripts\/install\.(?:sh|ps1))$/.test(url.pathname)) return url;
  if (url.hostname === 'github.com' && !url.search && /^\/LimeTip\/tapid\/releases\/download\/v[0-9.]+\/(?:tapid-[0-9.]+-(?:aarch64|x86_64)-(?:apple-darwin|pc-windows-msvc|unknown-linux-gnu)\.tar\.gz|SHA256SUMS|tapid-release-v1\.tsv(?:\.sig)?|install\.(?:sh|ps1))$/.test(url.pathname)) return url;
  if (url.hostname === 'release-assets.githubusercontent.com' && /^\/github-production-release-asset\/[0-9]+\/[a-f0-9-]+$/.test(url.pathname)
    && query.every(([key]) => ['sp', 'sv', 'sr', 'spr', 'se', 'skt', 'ske', 'sks', 'skv', 'skoid', 'sktid', 'rscd', 'rsct', 'sig', 'jwt', 'response-content-disposition', 'response-content-type'].includes(key))) return url;
  throw Error('disallowed public endpoint');
}

export class PublicTransport {
  private readonly request: typeof fetch;
  private readonly delay: (ms: number) => Promise<void>;
  private readonly now: () => number;
  private readonly deadline: number;
  private transferred = 0;
  private sourceTransferred = 0;
  private readonly transferBudget: number;
  private readonly sourceTransferBudget: number;

  constructor(options: Options = {}) {
    this.transferBudget = options.transferBudgetBytes ?? PUBLIC_LIMITS.aggregateBytes;
    this.sourceTransferBudget = options.sourceTransferBudgetBytes ?? PUBLIC_LIMITS.sourceTransferBytes;
    if (!Number.isSafeInteger(this.transferBudget) || this.transferBudget <= 0 || this.transferBudget > PUBLIC_LIMITS.aggregateBytes) throw Error('invalid public transfer budget');
    if (!Number.isSafeInteger(this.sourceTransferBudget) || this.sourceTransferBudget <= 0 || this.sourceTransferBudget > PUBLIC_LIMITS.sourceTransferBytes) throw Error('invalid tagged source transfer budget');
    this.request = options.fetch ?? fetch;
    this.delay = options.delay ?? (ms => new Promise(resolve => setTimeout(resolve, ms)));
    this.now = options.now ?? Date.now;
    this.deadline = this.now() + PUBLIC_LIMITS.overallMs;
  }

  remainingMs(): number {
    const remaining = this.deadline - this.now();
    if (remaining <= 0) throw Error('public verification deadline exceeded');
    return remaining;
  }

  async json(input: string): Promise<unknown> {
    const bytes = await this.bytes(input, publicUrl(input).hostname === 'raw.githubusercontent.com' ? PUBLIC_LIMITS.metadataBytes : PUBLIC_LIMITS.apiBytes);
    try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
    catch { throw Error('malformed public JSON response'); }
  }

  async bytes(input: string, limit: number): Promise<Uint8Array> {
    const initial = publicUrl(input);
    const sourceRequest = initial.hostname === 'raw.githubusercontent.com' || initial.pathname.includes('/git/trees/');
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > PUBLIC_LIMITS.archiveBytes) throw Error('invalid public response limit');
    for (let attempt = 1; attempt <= PUBLIC_LIMITS.attempts; attempt++) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), Math.min(PUBLIC_LIMITS.attemptMs, this.remainingMs()));
      let retry = false;
      let retryDelay = 1000 * attempt;
      try {
        let url = input;
        for (let redirects = 0; ; redirects++) {
          let response: Response;
          try {
            response = await this.request(publicUrl(url), {
              method: 'GET', redirect: 'manual', credentials: 'omit',
              headers: { Accept: 'application/json', 'Accept-Encoding': 'identity', 'User-Agent': 'tapid-public-verifier' },
              signal: controller.signal,
            });
          } catch { retry = true; break; }
          // Read even unsuccessful bodies through the same accounting boundary.
          let size = 0;
          const chunks: Uint8Array[] = [];
          const reader = response.body?.getReader();
          try {
            if (reader) for (;;) {
              let part: ReadableStreamReadResult<Uint8Array>;
              try { part = await reader.read(); } catch { retry = true; break; }
              if (part.done) break;
              size += part.value.byteLength;
              this.transferred += part.value.byteLength;
              if (sourceRequest) {
                this.sourceTransferred += part.value.byteLength;
                if (this.sourceTransferred > this.sourceTransferBudget) throw Error('tagged source transfer limit exceeded');
              }
              this.remainingMs();
              if (size > limit || this.transferred > this.transferBudget) throw Error('public response byte limit exceeded');
              chunks.push(part.value);
            }
          } finally { await reader?.cancel().catch(() => {}); }
          if (retry) break;
          const length = response.headers.get('content-length');
          if (length !== null && (!/^(0|[1-9][0-9]*)$/.test(length) || Number(length) !== size)) throw Error('public response length mismatch');
          if ([301, 302, 303, 307, 308].includes(response.status)) {
            if (redirects >= PUBLIC_LIMITS.redirects) throw Error('public redirect limit exceeded');
            const location = response.headers.get('location');
            if (!location) throw Error('missing public redirect destination');
            try { url = new URL(location, url).href; } catch { throw Error('invalid public redirect destination'); }
            publicUrl(url);
            continue;
          }
          if (response.status === 429 || response.status >= 500 && response.status <= 599) {
            retry = true;
            const after = response.headers.get('retry-after');
            if (after && /^[0-9]+$/.test(after)) retryDelay = Math.min(Number(after) * 1000, PUBLIC_LIMITS.retryDelayMs);
            break;
          }
          if (response.status !== 200) throw Error(`public request failed (HTTP ${response.status})`);
          const result = new Uint8Array(size);
          let offset = 0;
          for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.byteLength; }
          return result;
        }
      } finally { clearTimeout(timer); }
      if (!retry || attempt === PUBLIC_LIMITS.attempts) throw Error('public request retry limit exhausted');
      await this.delay(Math.min(retryDelay, PUBLIC_LIMITS.retryDelayMs, this.remainingMs()));
    }
    throw Error('public request retry limit exhausted');
  }

  /** Complete discovery or fail; never treat a truncated list as absence. */
  async list(input: string, field?: 'workflow_runs' | 'jobs'): Promise<unknown[]> {
    const entries: unknown[] = [];
    for (let page = 1; page <= PUBLIC_LIMITS.pages; page++) {
      const url = publicUrl(input);
      url.searchParams.set('per_page', '100');
      url.searchParams.set('page', String(page));
      const response = await this.json(url.href);
      const batch = field && response && typeof response === 'object' && !Array.isArray(response)
        ? (response as Record<string, unknown>)[field] : response;
      if (!Array.isArray(batch) || batch.length > 100) throw Error('malformed public discovery response');
      entries.push(...batch);
      if (batch.length < 100) return entries;
    }
    throw Error('public discovery page limit exceeded');
  }
}
