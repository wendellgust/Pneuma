'use strict';
// Wiring test: a fake `vscode` module + the REAL pneuma binary (skipped if it is not built).
const test = require('node:test');
const assert = require('node:assert');
const Module = require('module');
const fs = require('fs');
const os = require('os');
const path = require('path');
const cp = require('child_process');

const BIN = path.resolve(__dirname, '../../target/release/pneuma');
const have = fs.existsSync(BIN);

function makeVscode(project) {
  const log = { registered: {}, tasks: [], terminals: [], bars: [], contexts: {}, watchers: 0, trees: {} };
  const vscode = {
    EventEmitter: class { constructor() { this.event = () => ({ dispose() {} }); } fire() { log.fired = (log.fired || 0) + 1; } },
    TreeItem: class { constructor(label, state) { this.label = label; this.collapsibleState = state; } },
    ThemeIcon: class { constructor(id) { this.id = id; } },
    TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
    StatusBarAlignment: { Left: 1 },
    TaskScope: { Workspace: 2 },
    TaskGroup: { Build: 'build' },
    TaskRevealKind: { Always: 1 },
    ShellQuoting: { Strong: 2 },
    Uri: { file: (p) => ({ fsPath: p }) },
    ShellExecution: class { constructor(cmd, args, opts) { Object.assign(this, { cmd, args, opts }); } },
    Task: class { constructor(def, scope, name, source, exec, matchers) { Object.assign(this, { definition: def, scope, name, source, execution: exec, problemMatchers: matchers }); } },
    commands: {
      registerCommand: (id, fn) => { log.registered[id] = fn; return { dispose() {} }; },
      executeCommand: async (id, ...a) => { if (id === 'setContext') log.contexts[a[0]] = a[1]; },
    },
    window: {
      createStatusBarItem: () => { const it = { text: '', shown: false, show() { this.shown = true; }, hide() { this.shown = false; }, dispose() {} }; log.bars.push(it); return it; },
      createTerminal: (o) => { const t = { ...o, show() {}, dispose() {} }; log.terminals.push(t); return t; },
      registerTreeDataProvider: (id, p) => { log.trees[id] = p; return { dispose() {} }; },
      showQuickPick: async (entries) => entries[0],
      showInputBox: async () => 'x',
      showErrorMessage: async () => undefined,
    },
    workspace: {
      workspaceFolders: [{ uri: { fsPath: project } }],
      getConfiguration: () => ({ get: (k) => (k === 'path' ? BIN : true) }),
      createFileSystemWatcher: () => { log.watchers++; return { onDidChange: () => ({ dispose() {} }), onDidCreate: () => ({ dispose() {} }), onDidDelete: () => ({ dispose() {} }), dispose() {} }; },
      onDidChangeConfiguration: () => ({ dispose() {} }),
    },
    tasks: {
      registerTaskProvider: (type, p) => { log.provider = p; return { dispose() {} }; },
      executeTask: async (t) => { log.tasks.push(t); },
    },
  };
  return { vscode, log };
}

test('extension wiring against the real binary', { skip: !have && 'pneuma release binary not built' }, async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'pneuma-ext-'));
  cp.execFileSync(BIN, ['new', 'proj'], { cwd: tmp });
  const project = path.join(tmp, 'proj');
  const { vscode, log } = makeVscode(project);

  const origLoad = Module._load;
  Module._load = function (req, ...rest) { return req === 'vscode' ? vscode : origLoad.call(this, req, ...rest); };
  delete require.cache[require.resolve('../extension')];
  const ext = require('../extension');
  Module._load = origLoad;

  const ctx = { subscriptions: [] };
  const info = await ext.activate(ctx);
  assert.ok(info.ok, info.error);
  assert.ok(info.envs.some((e) => e.name === 'uno' && e.backend === 'native'));

  // every command in package.json is registered, and vice versa
  const pkg = require('../package.json');
  const declared = pkg.contributes.commands.map((c) => c.command).sort();
  assert.deepStrictEqual(Object.keys(log.registered).sort(), declared);
  for (const k of pkg.contributes.keybindings) assert.ok(declared.includes(k.command));
  assert.ok(pkg.activationEvents.some((a) => a.includes('pneuma.lua')));

  // status bar shows the default env and the buttons are visible
  assert.ok(log.bars[0].text.includes('uno'), log.bars[0].text);
  assert.ok(log.bars.every((b) => b.shown));
  assert.strictEqual(log.contexts['pneuma.active'], true);

  // build -> task running `pneuma build -e uno` with the gcc problem matcher
  await log.registered['pneuma.build']();
  const t = log.tasks[0];
  assert.strictEqual(t.execution.cmd, BIN);
  assert.deepStrictEqual(t.execution.args.map((a) => a.value), ['build', '-e', 'uno']);
  assert.deepStrictEqual(t.problemMatchers, ['$pneuma-gcc']);
  assert.strictEqual(t.execution.opts.cwd, project);

  // upload with a selected port includes -p; monitor is a real terminal running the binary
  ext._state.port = '/dev/ttyUSB0';
  await log.registered['pneuma.upload']();
  assert.deepStrictEqual(log.tasks[1].execution.args.map((a) => a.value), ['upload', '-e', 'uno', '-p', '/dev/ttyUSB0']);
  log.registered['pneuma.monitor']();
  assert.deepStrictEqual(log.terminals[0].shellArgs, ['monitor', '-e', 'uno', '-p', '/dev/ttyUSB0']);
  assert.strictEqual(log.terminals[0].shellPath, BIN);

  // task provider and env picker
  assert.deepStrictEqual(log.provider.provideTasks().map((x) => x.definition.command), ['build', 'upload', 'run']);
  await log.registered['pneuma.selectEnv']();
  assert.ok(log.bars[0].text.length > 0);

  // library command runs `lib add` and keeps the window open
  await log.registered['pneuma.addLibrary']();
  assert.deepStrictEqual(log.terminals.at(-1).shellArgs, ['lib', 'add', 'x', '--hold']);

  // sidebar: three views; tasks lists envs, clicking an env's Upload runs it with THAT env
  assert.deepStrictEqual(Object.keys(log.trees).sort(), ['pneuma.libs', 'pneuma.ports', 'pneuma.tasks']);
  for (const v of pkg.contributes.views.pneuma.filter((v) => v.id !== 'pneuma.welcome')) assert.ok(log.trees[v.id], v.id);
  await log.registered['pneuma.selectEnv']('uno');
  const tasks = log.trees['pneuma.tasks'];
  const groups = tasks.getChildren();
  assert.deepStrictEqual(groups.map((g) => g.label), ['Quick Access', 'Environments', 'Tools']);
  assert.strictEqual(tasks.getTreeItem(groups[0]).collapsibleState, 2);
  const envNode = groups[1].children.find((e) => e.label === 'uno');
  assert.ok(envNode.description.includes('selected'));
  const item = tasks.getTreeItem(envNode.children[1]);
  assert.deepStrictEqual(item.command, { command: 'pneuma.upload', title: 'Upload', arguments: ['uno'] });
  ext._state.port = 'auto';
  const other = groups[1].children.find((e) => e.label !== 'uno');
  await log.registered['pneuma.upload'](other.label);
  assert.deepStrictEqual(log.tasks.at(-1).execution.args.map((a) => a.value), ['upload', '-e', other.label]);
  await log.registered['pneuma.selectEnv']('uno');
  assert.ok(log.trees['pneuma.tasks'].getChildren()[1].children.find((e) => e.label === 'uno').description.includes('selected'));
  assert.strictEqual(log.trees['pneuma.ports'].getChildren()[0].label, 'auto');
  assert.ok(fs.existsSync(path.join(__dirname, '..', pkg.contributes.viewsContainers.activitybar[0].icon)));
  fs.mkdirSync(path.join(project, 'libs', 'MyLib'), { recursive: true });
  assert.deepStrictEqual(log.trees['pneuma.libs'].getChildren().map((n) => n.label), ['MyLib']);

  // a folder that is not a pneuma project hides the buttons
  const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'pneuma-empty-'));
  ext._state.info = { ok: false };
  vscode.workspace.workspaceFolders = [{ uri: { fsPath: empty } }];
  const i2 = await log.registered['pneuma.refresh']();
  assert.ok(!i2.ok);
  assert.ok(log.bars.every((b) => !b.shown));
  assert.strictEqual(log.contexts['pneuma.active'], false);
  assert.deepStrictEqual(log.trees['pneuma.tasks'].getChildren(), []);
});
