#!/usr/bin/env node
// corduroy-js build <entry> --out <file.wasm> [--capabilities a,b,...] [--wit <file>]
//
// Bundles a plugin module (JavaScript or TypeScript) with the SDK's runtime and builds it into
// a WebAssembly component that imports the named capabilities and nothing else: no WASI.

import { writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import { componentize } from '@bytecodealliance/componentize-js';
import { build } from 'esbuild';

import { defaultWit, specifier, world } from '../src/world.js';

const usage = 'usage: corduroy-js build <entry> --out <file.wasm> [--capabilities a,b] [--wit <file>]';

/** The capabilities the runtime routes globals to, when the plugin has them. */
const RUNTIME = ['log', 'clock', 'random'];

const identifier = (capability) => `cap_${capability.replace(/-/g, '_')}`;

/** The SDK's own sources. */
const SDK = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'src');

/** The module componentize-js builds: the runtime first, then the plugin, then the exports. */
function entry(plugin, capabilities) {
  const runtime = resolve(SDK, 'runtime.js');
  const imports = capabilities
    .filter((capability) => RUNTIME.includes(capability))
    .map((capability) => `import * as ${identifier(capability)} from '${specifier(capability)}';`);
  const handed = capabilities
    .filter((capability) => RUNTIME.includes(capability))
    .map((capability) => `${capability}: ${identifier(capability)}`);
  return `
import { exportsFor, install } from ${JSON.stringify(runtime)};
${imports.join('\n')}
import plugin from ${JSON.stringify(plugin)};
install({ ${handed.join(', ')} });
const exported = exportsFor(plugin);
export function activate(config) { return exported.activate(config); }
export function callTool(name, input) { return exported.callTool(name, input); }
export function onEvent(event) { exported.onEvent(event); }
export function promptPart() { return exported.promptPart(); }
export function gate(point, request) { return exported.gate(point, request); }
export function handleCall(call) { return exported.handleCall(call); }
`;
}

async function main() {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      out: { type: 'string', short: 'o' },
      capabilities: { type: 'string', short: 'c', default: '' },
      wit: { type: 'string' },
    },
  });
  const [command, source] = positionals;
  if (command !== 'build' || !source || !values.out) {
    throw new Error(usage);
  }
  const capabilities = values.capabilities
    .split(',')
    .map((capability) => capability.trim())
    .filter(Boolean);
  const wit = values.wit ? readFileSync(values.wit, 'utf8') : defaultWit();
  const witWorld = world(wit, capabilities);

  const bundled = await build({
    stdin: { contents: entry(resolve(source), capabilities), resolveDir: process.cwd(), loader: 'js' },
    bundle: true,
    format: 'esm',
    platform: 'neutral',
    target: 'es2022',
    external: ['corduroy:plugin/*'],
    // A plugin imports the SDK by name whether or not it installed it.
    alias: { 'corduroy-plugin': resolve(SDK, 'index.js') },
    write: false,
    logLevel: 'warning',
  });
  const { component } = await componentize(bundled.outputFiles[0].text, {
    witWorld,
    worldName: 'generated',
    // No WASI: the component imports only the capabilities above.
    disableFeatures: ['stdio', 'random', 'clocks', 'http', 'fetch-event'],
  });
  await writeFile(values.out, component);
  process.stdout.write(`built ${values.out} (${component.byteLength} bytes; capabilities: ${capabilities.join(', ') || 'none'})\n`);
}

main().catch((error) => {
  process.stderr.write(`${error.message ?? error}\n`);
  process.exit(1);
});
