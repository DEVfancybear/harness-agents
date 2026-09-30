#!/usr/bin/env node
'use strict';

// Starts the `ha` executable that ships in the platform package npm installed
// next to this one (`harness-agents-<platform>-<arch>`). This file only finds
// it and hands the terminal over; ha itself does everything else.

const { spawn } = require('node:child_process');
const path = require('node:path');

const PACKAGES = {
  'win32-x64': { name: 'harness-agents-win32-x64', executable: 'ha.exe' },
};

function fail(message) {
  process.stderr.write(`ha: ${message}\n`);
  process.exit(1);
}

const key = `${process.platform}-${process.arch}`;
const target = PACKAGES[key];
if (!target) {
  fail(
    `${key} is not supported. ha supports Windows x64 only for now; Linux builds from source but is not supported yet (https://github.com/DEVfancybear/harness-agents).`,
  );
}

let executable;
try {
  const manifest = require.resolve(`${target.name}/package.json`);
  executable = path.join(path.dirname(manifest), target.executable);
} catch {
  fail(
    `the ${target.name} package is missing. Reinstall without --omit=optional: npm install -g harness-agents`,
  );
}

// A Ctrl+C typed in the console reaches this process too; ha decides what it
// means (it pauses a goal, it cancels a turn), so this process must not exit
// before it does.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGBREAK']) {
  process.on(signal, () => {});
}

const child = spawn(executable, process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: false,
});
child.on('error', (error) => fail(`cannot start ${executable}: ${error.message}`));
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
