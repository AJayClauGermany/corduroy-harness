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

/** What a service returns: text as it is, anything else as JSON. */
export type TextOutput = string | number | boolean | object | null | undefined;

/** An image in a tool's result, as the `tools` capability's `call` returns it. */
export interface Image {
  /** What kind of image `data` is, such as `image/png`. */
  mimeType: string;
  /** The image's bytes, as they would be in a file. */
  data: Uint8Array;
}

/** One block of a tool's result, as the `tools` capability's `call` returns it. */
export type ContentBlock = { tag: 'text'; val: string } | { tag: 'image'; val: Image };

/**
 * One block of a tool's result, for a tool to return: text, or an image. Made with
 * `Content.text` and `Content.image`.
 */
export class Content {
  private constructor();
  readonly tag: 'text' | 'image';
  readonly val: string | Image;
  /** A block of text. Anything but a string is written as JSON. */
  static text(value: TextOutput): Content;
  /** An image of the kind `mimeType` names, such as `image/png`, from its bytes. */
  static image(mimeType: string, data: Uint8Array | ArrayBuffer | ArrayBufferView | number[]): Content;
  /**
   * The text of a tool's result: its text blocks joined by newlines, with an image standing
   * as a marker such as `[image: image/png, 1234 bytes]`.
   */
  static textOf(blocks: ReadonlyArray<ContentBlock | Content>): string;
}

/**
 * What a tool returns. A `Content`, or a non-empty array of them, is the result's blocks as
 * they are, so a tool can return images. Anything else is one block of text: a string as it
 * is, any other value (an array of anything but `Content` included) as JSON.
 */
export type ToolOutput = Content | Content[] | TextOutput;

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
  services?: Record<string, Record<string, (payload: any, call: ServiceCall) => TextOutput>>;
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
