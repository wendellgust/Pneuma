'use strict';
// Pure logic (no `vscode` import) so it can be unit-tested with plain node.

const path = require('path');

/** First existing candidate: the setting, PATH entries, then ~/.cargo/bin. */
function resolveBinary({ setting, pathEnv, home, exists, platform }) {
  const exe = platform === 'win32' ? 'pneuma.exe' : 'pneuma';
  const sep = platform === 'win32' ? ';' : ':';
  const candidates = [];
  if (setting && setting.trim()) candidates.push(setting.trim());
  for (const d of (pathEnv || '').split(sep).filter(Boolean)) candidates.push(path.join(d, exe));
  if (home) candidates.push(path.join(home, '.cargo', 'bin', exe));
  return candidates.find((c) => exists(c)) || null;
}

/** Parse `pneuma info` output into what the UI needs. Never throws. */
function parseInfo(text) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (e) {
    return { ok: false, error: `pneuma info did not return JSON: ${String(text).slice(0, 120)}`, envs: [], ports: [] };
  }
  const p = raw.project;
  return {
    ok: Boolean(p),
    error: p ? null : raw.project_error || 'no pneuma project here',
    version: raw.version,
    root: p ? p.root : null,
    defaultEnv: p ? p.default_env : null,
    envs: p ? p.envs : [],
    ports: raw.ports || [],
    remembered: p ? p.remembered_env_by_port || {} : {},
  };
}

/** The environment to use: the user's pick, else what worked with the chosen port, else default, else first valid. */
function chooseEnv(info, picked, port) {
  const valid = info.envs.filter((e) => e.ok).map((e) => e.name);
  if (picked && valid.includes(picked)) return picked;
  const rem = port && info.remembered[port];
  if (rem && valid.includes(rem)) return rem;
  if (info.defaultEnv && valid.includes(info.defaultEnv)) return info.defaultEnv;
  return valid[0] || null;
}

/** argv for a pneuma sub-command. `port` null/'auto' = let pneuma detect. */
function buildArgs(command, { env, port, extra } = {}) {
  const a = [command];
  if (env) a.push('-e', env);
  if (port && port !== 'auto' && command !== 'build') a.push('-p', port);
  return a.concat(extra || []);
}

/** `file:line:col: error: msg` from gcc (paths are already the user's files). */
function parseGccLine(line) {
  const m = /^(.*?):(\d+):(\d+):\s+(?:fatal )?(error|warning):\s+(.*)$/.exec(line);
  return m ? { file: m[1], line: Number(m[2]), column: Number(m[3]), severity: m[4], message: m[5] } : null;
}

function statusTexts(info, env, port) {
  return {
    env: `$(circuit-board) ${env || 'no env'}`,
    port: `$(plug) ${port && port !== 'auto' ? port : 'auto'}`,
    envTip: info.ok ? `Environment (backend: ${(info.envs.find((e) => e.name === env) || {}).backend || '?'}) — click to change` : info.error,
  };
}

module.exports = { resolveBinary, parseInfo, chooseEnv, buildArgs, parseGccLine, statusTexts };
