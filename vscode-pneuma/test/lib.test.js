'use strict';
const test = require('node:test');
const assert = require('node:assert');
const lib = require('../lib');

const INFO = JSON.stringify({
  version: '0.1.0',
  ports: [{ name: '/dev/ttyUSB0', desc: 'USB 10c4:ea60', id: '10c4:ea60:1', usb: true }],
  project: {
    root: '/p',
    default_env: 'uno',
    envs: [
      { name: 'esp32', ok: true, board: 'esp32dev', backend: 'arduino-cli' },
      { name: 'uno', ok: true, board: 'uno', backend: 'native' },
      { name: 'bad', ok: false, error: 'unknown board' },
    ],
    remembered_env_by_port: { '/dev/ttyUSB0': 'esp32' },
  },
});

test('resolveBinary: setting wins, then PATH, then ~/.cargo/bin', () => {
  const have = new Set(['/opt/x/pneuma', '/usr/bin/pneuma', '/home/u/.cargo/bin/pneuma']);
  const exists = (p) => have.has(p);
  const base = { pathEnv: '/bin:/usr/bin', home: '/home/u', exists, platform: 'linux' };
  assert.strictEqual(lib.resolveBinary({ ...base, setting: '/opt/x/pneuma' }), '/opt/x/pneuma');
  assert.strictEqual(lib.resolveBinary({ ...base, setting: '/nope' }), '/usr/bin/pneuma');
  assert.strictEqual(lib.resolveBinary({ ...base, setting: '', pathEnv: '/bin' }), '/home/u/.cargo/bin/pneuma');
  assert.strictEqual(lib.resolveBinary({ ...base, exists: () => false }), null);
  assert.strictEqual(lib.resolveBinary({ ...base, platform: 'win32', pathEnv: 'C:\\a;C:\\b', exists: (p) => p.endsWith('b\\pneuma.exe') || p.endsWith('b/pneuma.exe') }) !== null, true);
});

test('parseInfo: project, errors and garbage', () => {
  const i = lib.parseInfo(INFO);
  assert.ok(i.ok && i.root === '/p' && i.envs.length === 3 && i.ports.length === 1);
  const none = lib.parseInfo(JSON.stringify({ version: '1', ports: [], project_error: 'no project' }));
  assert.ok(!none.ok && none.error === 'no project');
  const bad = lib.parseInfo('Segmentation fault');
  assert.ok(!bad.ok && /did not return JSON/.test(bad.error));
});

test('chooseEnv: pick > remembered for port > default > first valid; never an invalid env', () => {
  const i = lib.parseInfo(INFO);
  assert.strictEqual(lib.chooseEnv(i, 'uno', '/dev/ttyUSB0'), 'uno');
  assert.strictEqual(lib.chooseEnv(i, null, '/dev/ttyUSB0'), 'esp32');
  assert.strictEqual(lib.chooseEnv(i, null, 'auto'), 'uno');
  assert.strictEqual(lib.chooseEnv(i, 'bad', 'auto'), 'uno');
  assert.strictEqual(lib.chooseEnv({ ...i, defaultEnv: null }, 'gone', 'auto'), 'esp32');
  assert.strictEqual(lib.chooseEnv({ ...i, envs: [], defaultEnv: null }, null, 'auto'), null);
});

test('buildArgs: env/port rules (build never takes a port)', () => {
  assert.deepStrictEqual(lib.buildArgs('build', { env: 'uno', port: '/dev/x' }), ['build', '-e', 'uno']);
  assert.deepStrictEqual(lib.buildArgs('upload', { env: 'uno', port: '/dev/x' }), ['upload', '-e', 'uno', '-p', '/dev/x']);
  assert.deepStrictEqual(lib.buildArgs('upload', { env: 'uno', port: 'auto' }), ['upload', '-e', 'uno']);
  assert.deepStrictEqual(lib.buildArgs('monitor', {}), ['monitor']);
});

test('parseGccLine', () => {
  const m = lib.parseGccLine("/w/src/main.cpp:8:16: error: 'undefined_fn' was not declared in this scope");
  assert.deepStrictEqual(m, { file: '/w/src/main.cpp', line: 8, column: 16, severity: 'error', message: "'undefined_fn' was not declared in this scope" });
  assert.strictEqual(lib.parseGccLine('/w/a.h:1:2: fatal error: x.h: No such file').severity, 'error');
  assert.strictEqual(lib.parseGccLine('Sketch uses 2124 bytes'), null);
  assert.strictEqual(lib.parseGccLine('/w/a.cpp:3:1: note: candidate'), null);
});
