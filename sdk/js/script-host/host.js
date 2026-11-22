// The script host: one component that runs automations written as JavaScript text.
//
// An automation is this component loaded with a configuration such as
//
//   {"name": "tidy", "script": "return { tools: { ... }, subscriptions: [...], onEvent(e) {} }"}
//
// The script is the body of a function called with `host` (every capability, by name),
// `ToolError` and `CallError`; it returns a plugin object of the same shape the SDK takes.
// Nothing is compiled per automation, so the agent can write one as a file, and each loaded
// instance is granted on its own: the host imports every capability, so an automation is
// given `Scope::none()` for each it may not use, and a call outside its grant is refused like
// any plugin's.
//
// Because it imports `tools` and `plugins`, the script host cannot gate `tool/call` or
// `plugin/load`.

import * as clock from 'corduroy:plugin/clock@0.1.0';
import * as events from 'corduroy:plugin/events@0.1.0';
import * as fsRead from 'corduroy:plugin/fs-read@0.1.0';
import * as fsWrite from 'corduroy:plugin/fs-write@0.1.0';
import * as http from 'corduroy:plugin/http@0.1.0';
import * as kernel from 'corduroy:plugin/kernel@0.1.0';
import * as llm from 'corduroy:plugin/llm@0.1.0';
import * as log from 'corduroy:plugin/log@0.1.0';
import * as logRead from 'corduroy:plugin/log-read@0.1.0';
import * as plugins from 'corduroy:plugin/plugins@0.1.0';
import * as random from 'corduroy:plugin/random@0.1.0';
import * as secret from 'corduroy:plugin/secret@0.1.0';
import * as service from 'corduroy:plugin/service@0.1.0';
import * as state from 'corduroy:plugin/state@0.1.0';
import * as tools from 'corduroy:plugin/tools@0.1.0';
import { CallError, ToolError } from 'corduroy-plugin';

const host = Object.freeze({
  clock,
  events,
  fsRead,
  fsWrite,
  http,
  kernel,
  llm,
  log,
  logRead,
  plugins,
  random,
  secret,
  service,
  state,
  tools,
});

const automation = {
  name: 'script',
  version: '0.1.0',

  activate(config) {
    if (typeof config.script !== 'string') {
      throw new Error('the script host needs `script` in its configuration: the body of a function returning a plugin');
    }
    // A restarted instance is activated again with the same configuration.
    const defined = new Function('host', 'ToolError', 'CallError', config.script)(host, ToolError, CallError);
    if (defined === null || typeof defined !== 'object') {
      throw new Error('the script must return a plugin object');
    }
    const { name: _ignored, activate: setup, ...parts } = defined;
    Object.assign(automation, parts, { name: config.name ?? 'script' });
    setup?.(config.config ?? {});
  },
};

export default automation;
