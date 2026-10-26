import assert from 'node:assert/strict';
import test from 'node:test';
import { PublicTransport } from './public_transport.ts';

test('public reads use fixed anonymous GET headers and recover transient responses', async () => {
  let calls = 0;
  const transport = new PublicTransport({
    fetch: async (_url, options) => {
      calls++;
      assert.equal(options?.method, 'GET');
      assert.equal(options?.redirect, 'manual');
      assert.deepEqual(options?.headers, { Accept: 'application/json', 'Accept-Encoding': 'identity', 'User-Agent': 'tapid-public-verifier' });
      return new Response(calls === 1 ? 'busy' : '{"ok":true}', { status: calls === 1 ? 503 : 200 });
    },
    delay: async () => {},
  });
  assert.deepEqual(await transport.json('https://api.github.com/repos/LimeTip/tapid/git/ref/tags/v0.0.11'), { ok: true });
  assert.equal(calls, 2);
});

test('rejects unsafe destinations before sending a request', async () => {
  let calls = 0;
  const transport = new PublicTransport({ fetch: async () => { calls++; return new Response('{}'); } });
  for (const url of [
    'http://api.github.com/repos/LimeTip/tapid/releases',
    'https://user:secret@api.github.com/repos/LimeTip/tapid/releases',
    'https://api.github.com:444/repos/LimeTip/tapid/releases',
    'https://example.com/repos/LimeTip/tapid/releases',
    'https://api.github.com/repos/LimeTip/tapid/releases?token=secret',
  ]) await assert.rejects(transport.json(url), /disallowed/);
  assert.equal(calls, 0);
});

test('redirect destinations are checked without leaking their contents', async () => {
  let calls = 0;
  const transport = new PublicTransport({ fetch: async () => {
    calls++;
    return new Response(null, { status: 302, headers: { location: 'https://secret:password@evil.example/leak' } });
  } });
  await assert.rejects(transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), error => {
    assert.equal((error as Error).message, 'disallowed public URL');
    return true;
  });
  assert.equal(calls, 1);
});

test('bounds streamed bodies even when the provider omits or lies about length', async () => {
  for (const headers of [{}, { 'content-length': '1' }]) {
    const transport = new PublicTransport({ fetch: async () => new Response('12345', { headers }) });
    await assert.rejects(transport.bytes('https://api.github.com/repos/LimeTip/tapid/releases', 4), /byte limit/);
  }
  const transport = new PublicTransport({ fetch: async () => new Response('12', { headers: { 'content-length': '3' } }) });
  await assert.rejects(transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), /length mismatch/);
});

test('exhausts retries but never retries malformed JSON or ordinary not-found responses', async () => {
  for (const [body, status, expectedCalls, message] of [
    ['busy', 429, 3, /retry limit/], ['busy', 503, 3, /retry limit/],
    ['missing', 404, 1, /HTTP 404/], ['broken', 200, 1, /malformed/],
  ] as const) {
    let calls = 0;
    const transport = new PublicTransport({ fetch: async () => { calls++; return new Response(body, { status }); }, delay: async () => {} });
    await assert.rejects(transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), message);
    assert.equal(calls, expectedCalls);
  }
});

test('caps redirect chains and complete discovery', async () => {
  let calls = 0;
  const redirects = new PublicTransport({ fetch: async () => { calls++; return new Response(null, { status: 302, headers: { location: '/repos/LimeTip/tapid/releases' } }); } });
  await assert.rejects(redirects.json('https://api.github.com/repos/LimeTip/tapid/releases'), /redirect limit/);
  assert.equal(calls, 6);
  calls = 0;
  const pages = new PublicTransport({ fetch: async url => {
    calls++;
    assert.equal(new URL(String(url)).searchParams.get('page'), String(calls));
    return Response.json(Array(100).fill({}));
  } });
  await assert.rejects(pages.list('https://api.github.com/repos/LimeTip/tapid/releases'), /page limit/);
  assert.equal(calls, 10);
});

test('deadline stops requests and retry delays are bounded', async () => {
  let now = 0;
  const delays: number[] = [];
  const transport = new PublicTransport({ now: () => now, fetch: async () => new Response(null, { status: 429, headers: { 'retry-after': '99999999' } }), delay: async ms => { delays.push(ms); now += 20 * 60_000; } });
  await assert.rejects(transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), /deadline/);
  assert.deepEqual(delays, [30_000]);
});

test('charges transient response bodies to the shared transfer budget', async () => {
  let calls = 0;
  const transport = new PublicTransport({ transferBudgetBytes: 5, delay: async () => {}, fetch: async () => {
    calls++;
    return new Response('busy', { status: 503 });
  } });
  await assert.rejects(transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), /byte limit/);
  assert.equal(calls, 2);
});

test('requests identity encoding so provider lengths describe the bytes being checked', async () => {
  const transport = new PublicTransport({ fetch: async (_url, options) => {
    assert.equal((options?.headers as Record<string, string>)['Accept-Encoding'], 'identity');
    return Response.json({ ok: true });
  }, delay: async () => {} });
  assert.deepEqual(await transport.json('https://api.github.com/repos/LimeTip/tapid/releases'), { ok: true });
});

test('accepts the bounded GitHub asset redirect route with Azure download parameters', async () => {
  let calls = 0;
  const transport = new PublicTransport({ fetch: async input => {
    calls++;
    if (calls === 1) return new Response(null, { status: 302, headers: { location: 'https://release-assets.githubusercontent.com/github-production-release-asset/123/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?sp=r&sv=2018-11-09&sr=b&spr=https&se=2026-10-26&rscd=attachment&rsct=application%2Foctet-stream&skoid=fixture&sktid=fixture&sig=fixture&jwt=fixture' } });
    assert.equal(new URL(String(input)).hostname, 'release-assets.githubusercontent.com');
    return new Response('fixture');
  } });
  assert.equal(Buffer.from(await transport.bytes('https://github.com/LimeTip/tapid/releases/download/v0.0.11/SHA256SUMS', 100)).toString(), 'fixture');
  assert.equal(calls, 2);
});
