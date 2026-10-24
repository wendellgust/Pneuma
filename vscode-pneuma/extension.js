'use strict';
const vscode = require('vscode');
const cp = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const lib = require('./lib');

const state = { info: { ok: false, envs: [], ports: [], remembered: {} }, env: null, port: 'auto', bin: null };
const items = {};
let terminals = [];

function root() {
  const f = vscode.workspace.workspaceFolders;
  return f && f.length ? f[0].uri.fsPath : null;
}

function binary(interactive = true) {
  const setting = vscode.workspace.getConfiguration('pneuma').get('path');
  const bin = lib.resolveBinary({
    setting,
    pathEnv: process.env.PATH,
    home: os.homedir(),
    exists: (p) => {
      try {
        return fs.statSync(p).isFile();
      } catch (e) {
        return false;
      }
    },
    platform: process.platform,
  });
  if (!bin && interactive) {
    vscode.window.showErrorMessage('Pneuma binary not found. Build it (cargo install --path crates/pneuma-cli) or set "pneuma.path".', 'Open settings').then((c) => {
      if (c) vscode.commands.executeCommand('workbench.action.openSettings', 'pneuma.path');
    });
  }
  state.bin = bin;
  return bin;
}

function currentEnv() {
  return lib.chooseEnv(state.info, state.env, state.port);
}

function libDirNames() {
  const r = root();
  if (!r || !state.info.ok) return [];
  try {
    const dir = path.join(r, state.info.libsDir || 'libs');
    return fs.readdirSync(dir, { withFileTypes: true }).filter((d) => d.isDirectory() && !d.name.startsWith('.')).map((d) => d.name).sort();
  } catch (e) {
    return [];
  }
}

/** One TreeDataProvider for the three sidebar views; the data comes from lib.treeFor. */
class PneumaTree {
  constructor(view) {
    this.view = view;
    this._emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this._emitter.event;
  }
  fire() {
    this._emitter.fire();
  }
  getChildren(node) {
    if (node) return node.children || [];
    return lib.treeFor(this.view, { info: state.info, env: currentEnv(), port: state.port, libDirs: libDirNames() });
  }
  getTreeItem(n) {
    const item = new vscode.TreeItem(n.label, n.children ? (n.id.startsWith('g.') ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed) : vscode.TreeItemCollapsibleState.None);
    item.id = n.id;
    if (n.description) item.description = n.description;
    if (n.icon) item.iconPath = new vscode.ThemeIcon(n.icon);
    if (n.command) item.command = { command: n.command, title: n.label, arguments: n.args || [] };
    return item;
  }
}
const trees = [];

function updateStatus() {
  for (const t of trees) t.fire();
  const show = vscode.workspace.getConfiguration('pneuma').get('statusBar') !== false;
  const t = lib.statusTexts(state.info, currentEnv(), state.port);
  items.env.text = t.env;
  items.env.tooltip = t.envTip;
  items.port.text = t.port;
  items.port.tooltip = 'Serial port — click to change';
  for (const it of Object.values(items)) (show && state.info.ok ? it.show() : it.hide());
  vscode.commands.executeCommand('setContext', 'pneuma.active', state.info.ok);
  vscode.commands.executeCommand('setContext', 'pneuma.noBinary', !state.info.ok && !state.bin);
}

function refresh() {
  return new Promise((resolve) => {
    const r = root();
    const bin = r && binary(false);
    if (!r || !bin) {
      state.info = { ok: false, error: r ? 'pneuma binary not found' : 'no folder open', envs: [], ports: [], remembered: {} };
      updateStatus();
      return resolve(state.info);
    }
    cp.execFile(bin, ['info', r], { cwd: r, timeout: 15000 }, (err, stdout) => {
      state.info = lib.parseInfo(err && !stdout ? String(err) : stdout);
      updateStatus();
      resolve(state.info);
    });
  });
}

const asEnv = (v) => (typeof v === 'string' ? v : undefined);

function taskFor(command, def = {}) {
  const r = root();
  const bin = binary();
  if (!r || !bin) return null;
  const env = def.env || currentEnv();
  const port = def.port || state.port;
  const args = lib.buildArgs(command, { env, port });
  const exec = new vscode.ShellExecution(bin, args.map((value) => ({ value, quoting: vscode.ShellQuoting.Strong })), { cwd: r });
  const task = new vscode.Task({ type: 'pneuma', command, env, port }, vscode.TaskScope.Workspace, `pneuma ${args.join(' ')}`, 'pneuma', exec, command === 'monitor' ? [] : ['$pneuma-gcc']);
  task.presentationOptions = { reveal: vscode.TaskRevealKind.Always, clear: command === 'build' };
  if (command === 'build') task.group = vscode.TaskGroup.Build;
  return task;
}

function runTask(command, env) {
  const t = taskFor(command, { env });
  if (t) vscode.tasks.executeTask(t);
}

/** Interactive things (monitor, tui, lib add...) go in a real terminal running the binary directly. */
function runInTerminal(name, args, { hold = false, anyDir = false } = {}) {
  const bin = binary();
  const r = root() || (anyDir ? require('os').homedir() : null);
  if (!bin || !r) return;
  const term = vscode.window.createTerminal({ name, shellPath: bin, shellArgs: hold ? args.concat('--hold') : args, cwd: r });
  terminals.push(term);
  term.show();
}

async function pick(title, entries) {
  const sel = await vscode.window.showQuickPick(entries, { title, placeHolder: title });
  return sel;
}

const commands = {
  'pneuma.build': (env) => runTask('build', asEnv(env)),
  'pneuma.upload': (env) => runTask('upload', asEnv(env)),
  'pneuma.run': (env) => runTask('run', asEnv(env)),
  'pneuma.monitor': (env) => runInTerminal('Pneuma Monitor', lib.buildArgs('monitor', { env: asEnv(env) || currentEnv(), port: state.port })),
  'pneuma.selectEnv': async (name) => {
    if (asEnv(name)) {
      state.env = name;
      return updateStatus();
    }
    await refresh();
    const names = state.info.envs.filter((e) => e.ok).map((e) => ({ label: e.name, description: `${e.board} · ${e.backend}` }));
    const sel = await pick('Pneuma environment', names);
    if (sel) {
      state.env = sel.label;
      updateStatus();
    }
  },
  'pneuma.selectPort': async (name) => {
    if (asEnv(name)) {
      state.port = name;
      const rem = state.info.remembered[name];
      if (rem) state.env = rem;
      return updateStatus();
    }
    await refresh();
    const entries = [{ label: 'auto', description: 'detect the single connected board' }].concat(state.info.ports.map((p) => ({ label: p.name, description: p.desc })));
    const sel = await pick('Serial port', entries);
    if (sel) {
      state.port = sel.label;
      const rem = state.info.remembered[sel.label];
      if (rem) state.env = rem;
      updateStatus();
    }
  },
  'pneuma.new': async () => {
    const bin = binary();
    if (!bin) return;
    const name = await vscode.window.showInputBox({ prompt: 'New project name', validateInput: (v) => (/^[\w.-]+$/.test(v || '') ? null : 'letters, digits, _ - . only') });
    if (!name) return;
    const dest = await vscode.window.showOpenDialog({ canSelectFolders: true, canSelectFiles: false, openLabel: 'Create here' });
    if (!dest || !dest.length) return;
    const parent = dest[0].fsPath;
    cp.execFile(bin, ['new', name], { cwd: parent }, (err, stdout, stderr) => {
      if (err) return vscode.window.showErrorMessage(`pneuma new failed: ${stderr || err.message}`);
      vscode.commands.executeCommand('vscode.openFolder', vscode.Uri.file(path.join(parent, name)));
    });
  },
  'pneuma.addLibrary': async () => {
    const n = await vscode.window.showInputBox({ prompt: 'Library name (Arduino registry), e.g. "DHT sensor library" or "Servo@1.2.1"' });
    if (n) runInTerminal('Pneuma Library', ['lib', 'add', n], { hold: true });
  },
  'pneuma.searchLibrary': async () => {
    const q = await vscode.window.showInputBox({ prompt: 'Search the Arduino library registry' });
    if (q) runInTerminal('Pneuma Library', ['lib', 'search', q], { hold: true });
  },
  'pneuma.setup': () => runInTerminal('Pneuma Setup', ['setup'], { hold: true }),
  'pneuma.setupFreertos': () => runInTerminal('Pneuma FreeRTOS', ['setup', '--freertos'], { hold: true, anyDir: true }),
  'pneuma.setupExtras': () => runInTerminal('Pneuma Extras', ['setup', '--extras'], { hold: true, anyDir: true }),
  'pneuma.revealPath': (p) => p && vscode.commands.executeCommand('revealFileInOS', vscode.Uri.file(p)),
  'pneuma.openTui': () => runInTerminal('Pneuma', ['tui']),
  'pneuma.refresh': () => refresh(),
  'pneuma.revealLib': (name) => {
    const r = root();
    if (r && asEnv(name)) vscode.commands.executeCommand('revealInExplorer', vscode.Uri.file(path.join(r, state.info.libsDir || 'libs', name)));
  },
};

function activate(context) {
  for (const [id, fn] of Object.entries(commands)) context.subscriptions.push(vscode.commands.registerCommand(id, fn));

  const mk = (key, priority, command) => {
    const it = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, priority);
    it.command = command;
    items[key] = it;
    context.subscriptions.push(it);
    return it;
  };
  mk('env', 100, 'pneuma.selectEnv');
  mk('port', 99, 'pneuma.selectPort');
  mk('build', 98, 'pneuma.build').text = '$(check) Build';
  mk('upload', 97, 'pneuma.upload').text = '$(cloud-upload) Upload';
  mk('monitor', 96, 'pneuma.monitor').text = '$(plug) Monitor';
  items.build.tooltip = 'Pneuma: build';
  items.upload.tooltip = 'Pneuma: upload';
  items.monitor.tooltip = 'Pneuma: serial monitor';

  for (const view of ['tasks', 'ports', 'libs']) {
    const t = new PneumaTree(view);
    trees.push(t);
    context.subscriptions.push(vscode.window.registerTreeDataProvider(`pneuma.${view}`, t));
  }

  context.subscriptions.push(
    vscode.tasks.registerTaskProvider('pneuma', {
      provideTasks: () => ['build', 'upload', 'run'].map((c) => taskFor(c)).filter(Boolean),
      resolveTask: (t) => (t.definition && t.definition.command ? taskFor(t.definition.command, t.definition) : undefined),
    })
  );

  const w = vscode.workspace.createFileSystemWatcher('**/{pneuma.lua,bmu.lua,platformio.ini}');
  for (const ev of ['onDidChange', 'onDidCreate', 'onDidDelete']) context.subscriptions.push(w[ev](() => refresh()));
  context.subscriptions.push(w);
  context.subscriptions.push(vscode.workspace.onDidChangeConfiguration((e) => e.affectsConfiguration('pneuma') && refresh()));

  return refresh();
}

function deactivate() {
  for (const t of terminals) {
    try {
      t.dispose();
    } catch (e) {}
  }
}

module.exports = { activate, deactivate, _state: state, _commands: commands, _trees: trees };
