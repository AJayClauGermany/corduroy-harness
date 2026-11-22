// Types for corduroy plugins written in TypeScript. `corduroy-js build` bundles `.ts`
// entries as they are; these declarations let an editor check a plugin's shape.

/** One entry of the harness event stream. */
export interface Event {
  id: bigint;
  /** The session the event belongs to; none for events about the harness itself. */
  session?: string;
  /** `user`, `agent`, `environment`, `host` or `plugin:<name>`. */
  source: string;
  cause?: bigint;
  topic: string;
  /** JSON text. */
  payload: string;
}

/** A call to one of this plugin's services. */
export interface ServiceCall {
  caller: string;
  service: string;
  method: string;
  payload: string;
}

/** What a tool returns: text as it is, anything else as JSON. */
export type ToolOutput = string | number | boolean | object | null | undefined;

export interface Tool<Input = any> {
  description: string;
  /** JSON Schema for the input object. Defaults to `{ "type": "object" }`. */
  input?: object;
  /** Calling twice is the same as calling once, so the host may repeat it after a crash. */
  replaySafe?: boolean;
  /** The tool only looks and changes nothing. */
  readOnly?: boolean;
  run(input: Input): ToolOutput;
}

/** A gate's answer: run it, ask a person first, or refuse. */
export type Verdict = 'allow' | { allow: true } | { ask: string } | { deny: string };

export interface Plugin {
  name: string;
  version?: string;
  /** Called with the parsed configuration when the plugin starts, and again after a trap. */
  activate?(config: any): void;
  tools?: Record<string, Tool>;
  /** Topic globs to receive in `onEvent`. */
  subscriptions?: string[];
  onEvent?(event: Event): void;
  /** Text for the system prompt of the next model request, or nothing. */
  prompt?(): string | undefined;
  /** Gates by decision point, such as `tool/call`. A gate that throws denies. */
  gates?: Record<string, (request: any) => Verdict>;
  /** Services by name, each a set of methods taking the parsed payload. */
  services?: Record<string, Record<string, (payload: any, call: ServiceCall) => ToolOutput>>;
}

/** Returns `plugin` unchanged, typed. */
export function definePlugin<P extends Plugin>(plugin: P): P;

export class ToolError extends Error {
  constructor(kind: 'invalid-input' | 'failed' | 'denied' | 'unknown-tool', message: string);
  kind: string;
}

export class CallError extends Error {
  constructor(kind: 'unknown' | 'invalid-input' | 'failed' | 'denied', message: string);
  kind: string;
}
