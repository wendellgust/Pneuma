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
    libsDir: p ? p.libs_dir : null,
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

/**
 * Sidebar trees as plain data: { id, label, description?, icon?, command?, args?, children? }.
 * `view` is 'tasks' | 'ports' | 'libs'; `libDirs` = folder names found in the project's libs dir.
 */
function treeFor(view, { info, env, port, libDirs = [] }) {
  const cmd = (id, label, command, icon, args) => ({ id, label, command, icon, args });
  if (!info.ok) return [];
  if (view === 'tasks') {
    const quick = [
      cmd('q.build', 'Build', 'pneuma.build', 'check'),
      cmd('q.upload', 'Upload', 'pneuma.upload', 'cloud-upload'),
      cmd('q.run', 'Build, Upload and Monitor', 'pneuma.run', 'play'),
      cmd('q.monitor', 'Serial Monitor', 'pneuma.monitor', 'plug'),
      cmd('q.tui', 'Terminal UI', 'pneuma.openTui', 'terminal'),
    ];
    const envs = info.envs.map((e) => {
      const node = { id: `env.${e.name}`, label: e.name, icon: e.name === env ? 'pass-filled' : 'circuit-board' };
      if (!e.ok) return { ...node, icon: 'warning', description: e.error || 'invalid environment' };
      return {
        ...node,
        description: `${e.board} · ${e.backend}${e.name === env ? ' · selected' : ''}`,
        command: 'pneuma.selectEnv',
        args: [e.name],
        children: [
          cmd(`env.${e.name}.build`, 'Build', 'pneuma.build', 'check', [e.name]),
          cmd(`env.${e.name}.upload`, 'Upload', 'pneuma.upload', 'cloud-upload', [e.name]),
          cmd(`env.${e.name}.run`, 'Build, Upload and Monitor', 'pneuma.run', 'play', [e.name]),
          cmd(`env.${e.name}.monitor`, 'Serial Monitor', 'pneuma.monitor', 'plug', [e.name]),
        ],
      };
    });
    const tools = [
      cmd('t.addlib', 'Add Library', 'pneuma.addLibrary', 'add'),
      cmd('t.searchlib', 'Search Libraries', 'pneuma.searchLibrary', 'search'),
      cmd('t.setup', 'Install Toolchain (AVR)', 'pneuma.setup', 'cloud-download'),
      cmd('t.refresh', 'Refresh', 'pneuma.refresh', 'refresh'),
    ];
    return [
      { id: 'g.quick', label: 'Quick Access', icon: 'zap', children: quick },
      { id: 'g.envs', label: 'Environments', icon: 'circuit-board', children: envs },
      { id: 'g.tools', label: 'Tools', icon: 'tools', children: tools },
    ];
  }
  if (view === 'ports') {
    const auto = { id: 'p.auto', label: 'auto', icon: !port || port === 'auto' ? 'pass-filled' : 'circle-outline', description: 'detect the single connected board', command: 'pneuma.selectPort', args: ['auto'] };
    const ports = info.ports.map((p) => ({
      id: `p.${p.name}`,
      label: p.name,
      icon: p.name === port ? 'pass-filled' : 'plug',
      description: [p.desc, info.remembered[p.name] ? `last env: ${info.remembered[p.name]}` : null].filter(Boolean).join(' · '),
      command: 'pneuma.selectPort',
      args: [p.name],
    }));
    return [auto].concat(ports);
  }
  if (view === 'libs') {
    return libDirs.map((d) => ({ id: `l.${d}`, label: d, icon: 'library', command: 'pneuma.revealLib', args: [d] }));
  }
  return [];
}

module.exports = { treeFor, resolveBinary, parseInfo, chooseEnv, buildArgs, parseGccLine, statusTexts };
