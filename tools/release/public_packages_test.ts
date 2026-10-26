import assert from 'node:assert/strict';
import test from 'node:test';
import { verifyPublicPackages } from './public_packages.ts';
import { PublicTransport } from './public_transport.ts';

const metadata = { packages: [
  { name: 'tapid', version: '0.0.11', dependencies: ['tapid-core'] },
  { name: 'tapid-core', version: '0.0.3' },
  { name: 'private-fixture', version: '0.0.1', publish: [] },
] };
function transport(response: (name: string, version: string) => Response) {
  return new PublicTransport({ delay: async () => {}, fetch: async (input, init) => {
    const url = new URL(String(input));
    assert.equal(url.hostname, 'crates.io');
    assert.equal(init?.method, 'GET');
    assert.equal(new Headers(init?.headers).has('authorization'), false);
    const [, , , , name, version] = url.pathname.split('/');
    return response(name, version);
  } });
}

test('verifies the complete publication matrix including older dependency versions', async () => {
  const observed = await verifyPublicPackages(transport((name, num) => Response.json({ version: { crate: name, num, yanked: false } })), metadata);
  assert.deepEqual(observed, [
    { name: 'tapid-core', version: '0.0.3', yanked: false },
    { name: 'tapid', version: '0.0.11', yanked: false },
  ]);
});

test('rejects unpublished, yanked, and inconsistent version responses', async () => {
  for (const response of [
    () => new Response('', { status: 404 }),
    (crate: string, num: string) => Response.json({ version: { crate, num, yanked: true } }),
    (crate: string, num: string) => Response.json({ version: { crate, num } }),
    (crate: string) => Response.json({ version: { crate, num: '9.9.9', yanked: false } }),
    (_: string, num: string) => Response.json({ version: { crate: 'other', num, yanked: false } }),
    () => Response.json({ version: null }),
  ]) await assert.rejects(verifyPublicPackages(transport(response), metadata));
});

test('recovers from transient public registry failure within the shared retry budget', async () => {
  let calls = 0;
  const result = await verifyPublicPackages(transport((crate, num) => ++calls === 1
    ? new Response('', { status: 503 }) : Response.json({ version: { crate, num, yanked: false } })), metadata);
  assert.equal(result.length, 2);
  assert.equal(calls, 3);
});

test('invalid metadata fails before constructing registry URLs', async () => {
  for (const packages of [
    [{ name: '../tapid', version: '0.0.11' }],
    [{ name: 'tapid', version: '0.0.11' }, { name: 'tapid', version: '0.0.12' }],
    [{ name: 'tapid', version: '0.0.11?token=secret' }],
  ]) await assert.rejects(verifyPublicPackages(transport(() => assert.fail('no network')), { packages }));
});

test('preserves prerelease and build identities from tagged Cargo dependency metadata', async () => {
  const packages = { packages: [
    { name: 'tapid', version: '0.0.11', dependencies: ['tapid-core'] },
    { name: 'tapid-core', version: '1.2.3-rc.1+build.7' },
  ] };
  const result = await verifyPublicPackages(transport((crate, num) => Response.json({ version: { crate, num, yanked: false } })), packages);
  assert.equal(result[0].version, '1.2.3-rc.1+build.7');
});
