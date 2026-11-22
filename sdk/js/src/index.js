// Write corduroy plugins in JavaScript or TypeScript.
//
// A plugin module's default export is a plain object describing the plugin; `corduroy-js
// build` turns it into a component. Host capabilities are imported directly, for example
// `import { read } from 'corduroy:plugin/fs-read@0.1.0'`, and the plugin must be built with
// each one it imports (`--capabilities fs-read,...`).

export { CallError, Content, ToolError } from './runtime.js';

/** Returns `plugin` unchanged; it exists so editors can check the shape of a plugin. */
export function definePlugin(plugin) {
  return plugin;
}
