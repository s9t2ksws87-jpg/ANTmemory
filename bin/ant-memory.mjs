#!/usr/bin/env node
// Compatibility entry point for existing Node-based MCP configurations.
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = process.env.ANT_MEMORY_ROOT || path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const exe = path.join(root, 'runtime', 'bin', process.platform === 'win32' ? 'antmemory.exe' : 'antmemory');
if (!existsSync(exe)) {
  process.stderr.write('ANTmemory 4 needs the native executable. Extract the release ZIP here, or build and install it as described in README.md.\n');
  process.exit(1);
}
if (process.argv.length > 2) {
  process.stderr.write('The compatibility entry point accepts no options. Configure the native executable with args ["serve"]; see README.md for routing and maintenance.\n');
  process.exit(1);
}
const child = spawn(exe, ['serve'], { stdio: 'inherit', windowsHide: true, env: { ...process.env, ANT_MEMORY_ROOT: root } });
child.on('error', error => { process.stderr.write(`Cannot start ANTmemory: ${error.message}\n`); process.exit(1); });
child.on('exit', (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
