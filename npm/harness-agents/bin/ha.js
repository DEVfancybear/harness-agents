#!/usr/bin/env node
'use strict';

// Starts the `ha.exe` that ships in this package and hands the terminal over;
// ha itself does everything else. `os`/`cpu` in package.json make npm refuse the
// install anywhere but Windows x64, so there is no other platform to handle here.

const { spawn } = require('node:child_process');
const path = require('node:path');

const executable = path.join(__dirname, '..', 'ha.exe');

// A Ctrl+C typed in the console reaches this process too; ha decides what it
// means (it pauses a goal, it cancels a turn), so this process must not exit
// before it does.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGBREAK']) {
  process.on(signal, () => {});
}

const child = spawn(executable, process.argv.slice(2), { stdio: 'inherit' });
child.on('error', (error) => {
  process.stderr.write(`ha: cannot start ${executable}: ${error.message}\n`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
