// The part of a JavaScript plugin that runs inside the component.
//
// Built without WASI, a component has no console, clock or random source of its own: logging
// is silently dropped, `Date.now()` returns the moment the component was built,
// `performance.now()` and `fetch` trap, and `Math.random()` repeats the same sequence in every
// instance. Importing this module routes each of them to a host capability instead, where
// the plugin was granted it, and makes the rest fail with an error that says what to grant.
// The host interfaces themselves are handed over by `install`, because which of them exist
// depends on the world the plugin was built against.

/** A tool failed. `kind` is one of `invalid-input`, `failed`, `denied`, `unknown-tool`. */
export class ToolError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = 'ToolError';
    this.kind = kind;
  }
}

/** A service call failed. `kind` is one of `unknown`, `invalid-input`, `failed`, `denied`. */
export class CallError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = 'CallError';
    this.kind = kind;
  }
}

/** The host interfaces the runtime itself uses: `log`, `clock` and `random`, if granted. */
let host = {};

/** Hand the runtime the host interfaces the plugin's world imports. */
export function install(imports) {
  host = imports;
}

/** The host error inside an exception thrown by a host call, if it is one. */
function hostError(error) {
  const payload = error?.payload;
  if (payload && (payload.tag === 'denied' || payload.tag === 'failed')) {
    return payload;
  }
  return undefined;
}

function describe(error) {
  if (error instanceof Error) {
    return `${error.name}: ${error.message}`;
  }
  return String(error);
}

function missing(capability, what) {
  return new Error(`${what} needs the \`${capability}\` capability, which this plugin was not built with`);
}

// --- Logging -------------------------------------------------------------------------------

function format(args) {
  return args
    .map((arg) => {
      if (typeof arg === 'string') {
        return arg;
      }
      if (arg instanceof Error) {
        return describe(arg);
      }
      try {
        return JSON.stringify(arg, (_key, value) => (typeof value === 'bigint' ? value.toString() : value));
      } catch {
        return String(arg);
      }
    })
    .join(' ');
}

function write(level, args) {
  if (!host.log) {
    return;
  }
  try {
    host.log.write(level, format(args));
  } catch {
    // A refused log line has nowhere else to go.
  }
}

const console = {
  debug: (...args) => write('debug', args),
  log: (...args) => write('info', args),
  info: (...args) => write('info', args),
  warn: (...args) => write('warn', args),
  error: (...args) => write('error', args),
  trace: (...args) => write('debug', args),
};

// --- Time ----------------------------------------------------------------------------------

function nowMs() {
  if (!host.clock) {
    throw missing('clock', 'The time');
  }
  const { seconds, nanoseconds } = host.clock.now();
  return Number(seconds) * 1000 + Math.floor(nanoseconds / 1e6);
}

function monotonicMs() {
  if (!host.clock) {
    throw missing('clock', 'performance.now()');
  }
  return Number(host.clock.monotonic()) / 1e6;
}

const BuiltDate = globalThis.Date;

/** `Date`, whose current time comes from the host's wall clock. */
class HostDate extends BuiltDate {
  constructor(...args) {
    if (args.length === 0) {
      super(nowMs());
    } else {
      super(...args);
    }
  }

  static now() {
    return nowMs();
  }
}

// --- Randomness ----------------------------------------------------------------------------

let pool = new Uint8Array(0);
let used = 0;

function randomBytes(count) {
  if (!host.random) {
    throw missing('random', 'Randomness');
  }
  const out = new Uint8Array(count);
  let filled = 0;
  while (filled < count) {
    if (used >= pool.length) {
      pool = host.random.getBytes(Math.max(256, Math.min(65536, count - filled)));
      used = 0;
    }
    const take = Math.min(count - filled, pool.length - used);
    out.set(pool.subarray(used, used + take), filled);
    used += take;
    filled += take;
  }
  return out;
}

/** A uniform float in [0, 1) from 53 random bits. */
function random() {
  const bytes = randomBytes(7);
  let high = 0;
  for (let index = 0; index < 3; index += 1) {
    high = high * 256 + bytes[index];
  }
  let low = 0;
  for (let index = 3; index < 7; index += 1) {
    low = low * 256 + bytes[index];
  }
  // 21 bits from `high`, 32 from `low`.
  return ((high & 0x1fffff) * 2 ** 32 + low) / 2 ** 53;
}

function getRandomValues(array) {
  const bytes = randomBytes(array.byteLength);
  new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(bytes);
  return array;
}

// --- Installing the globals ----------------------------------------------------------------

function define(target, name, value) {
  try {
    Object.defineProperty(target, name, { value, writable: true, configurable: true });
  } catch {
    // A global the engine will not let go of keeps its own behaviour.
  }
}

define(globalThis, 'console', console);
define(globalThis, 'Date', HostDate);
define(Math, 'random', random);
define(globalThis, 'performance', { now: monotonicMs });
if (globalThis.crypto) {
  define(globalThis.crypto, 'getRandomValues', getRandomValues);
} else {
  define(globalThis, 'crypto', { getRandomValues });
}
define(globalThis, 'fetch', () => {
  throw new Error('fetch is not available in a plugin: use the `http` capability (corduroy:plugin/http)');
});

// --- The exports ---------------------------------------------------------------------------

const TOOL_ERRORS = ['unknown-tool', 'invalid-input', 'failed', 'denied'];
const CALL_ERRORS = ['unknown', 'invalid-input', 'failed', 'denied'];

function toolError(error) {
  if (error instanceof ToolError && TOOL_ERRORS.includes(error.kind)) {
    return { tag: error.kind, val: error.message };
  }
  const fromHost = hostError(error);
  if (fromHost) {
    return { tag: fromHost.tag, val: fromHost.val };
  }
  return { tag: 'failed', val: describe(error) };
}

function callError(error) {
  if (error instanceof CallError && CALL_ERRORS.includes(error.kind)) {
    return { tag: error.kind, val: error.message };
  }
  const fromHost = hostError(error);
  if (fromHost) {
    return { tag: fromHost.tag, val: fromHost.val };
  }
  return { tag: 'failed', val: describe(error) };
}

/** What a tool or service returned, as the text the host passes on. */
function text(value) {
  if (value === undefined || value === null) {
    return '';
  }
  return typeof value === 'string' ? value : JSON.stringify(value);
}

function parse(json, fail) {
  try {
    return json.trim() === '' ? {} : JSON.parse(json);
  } catch (error) {
    throw fail(error.message);
  }
}

function verdict(answer) {
  if (answer === 'allow' || answer?.allow === true) {
    return { tag: 'allow' };
  }
  if (typeof answer?.ask === 'string') {
    return { tag: 'ask', val: answer.ask };
  }
  if (typeof answer?.deny === 'string') {
    return { tag: 'deny', val: answer.deny };
  }
  return { tag: 'deny', val: 'the gate gave no answer it could stand by' };
}

/**
 * The exports of the plugin world for `plugin`, an object such as:
 *
 * ```js
 * {
 *   name: 'notes', version: '0.1.0',
 *   activate(config) {},                                       // optional
 *   tools: { note: { description, input, readOnly, replaySafe, run(input) {} } },
 *   subscriptions: ['turn/end'], onEvent(event) {},            // optional
 *   prompt() { return 'text for the system prompt'; },         // optional
 *   gates: { 'tool/call': (request) => 'allow' | { ask } | { deny } },
 *   services: { notes: { list(payload, call) {} } },
 * }
 * ```
 */
export function exportsFor(plugin) {
  // Read on every call, so that `activate` may fill the plugin in (the script host does).
  const tools = () => plugin.tools ?? {};
  const gates = () => plugin.gates ?? {};
  const services = () => plugin.services ?? {};
  return {
    activate(config) {
      const parsed = parse(config, (message) => `bad config: ${message}`);
      try {
        plugin.activate?.(parsed);
      } catch (error) {
        throw describe(error);
      }
      return {
        name: plugin.name,
        version: plugin.version ?? '0.0.0',
        tools: Object.entries(tools()).map(([name, tool]) => ({
          name,
          description: tool.description ?? '',
          inputSchema: JSON.stringify(tool.input ?? { type: 'object' }),
          replaySafe: tool.replaySafe === true,
          readOnly: tool.readOnly === true,
        })),
        subscriptions: plugin.subscriptions ?? [],
        prompt: typeof plugin.prompt === 'function',
        gates: Object.keys(gates()),
        provides: Object.keys(services()),
      };
    },

    callTool(name, input) {
      const tool = tools()[name];
      if (!tool) {
        throw { tag: 'unknown-tool', val: name };
      }
      let parsed;
      try {
        parsed = parse(input, (message) => new ToolError('invalid-input', message));
        return text(tool.run(parsed));
      } catch (error) {
        throw toolError(error);
      }
    },

    onEvent(event) {
      try {
        plugin.onEvent?.(event);
      } catch (error) {
        write('error', [`onEvent failed: ${describe(error)}`]);
      }
    },

    promptPart() {
      try {
        const part = plugin.prompt?.();
        return typeof part === 'string' ? part : undefined;
      } catch (error) {
        write('error', [`prompt failed: ${describe(error)}`]);
        return undefined;
      }
    },

    gate(point, request) {
      const decide = gates()[point];
      if (typeof decide !== 'function') {
        return { tag: 'deny', val: `this plugin does not guard \`${point}\`` };
      }
      try {
        return verdict(decide(JSON.parse(request)));
      } catch (error) {
        // A gate that cannot answer fails closed.
        return { tag: 'deny', val: `the gate failed: ${describe(error)}` };
      }
    },

    handleCall(call) {
      const service = services()[call.service];
      const method = service?.[call.method];
      if (typeof method !== 'function') {
        throw { tag: 'unknown', val: `${call.service}.${call.method}` };
      }
      try {
        const payload = parse(call.payload, (message) => new CallError('invalid-input', message));
        return text(method.call(service, payload, call));
      } catch (error) {
        throw callError(error);
      }
    },
  };
}
