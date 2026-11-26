// The page of the `viewer` add-on: reads the event files beside it and shows them.
//
// The add-on writes `index.js` (which chunks there are) and `events-NNNNNN.js` (the events),
// each a script that calls one of the two functions below. Everything shown comes from a
// model, a tool or a person, so it is only ever put on the page as text.
'use strict';
(() => {
  const FORMAT = 1;
  const POLL_MS = 2000;
  /** How many timeline items, or event rows, are drawn before "show earlier" is needed. */
  const WINDOW = 300;
  /** Characters of one output drawn; the log has the rest. */
  const MAX_SHOWN = 40000;

  const state = {
    index: null,
    chunks: [],
    events: [],
    model: null,
    /** Keys of the parts a person opened, so that a redraw keeps them open. */
    open: new Set(),
    /** How many more than WINDOW to draw, by view. */
    extra: {},
    filter: '',
    arrived: null,
    loading: false,
    drawn: '',
  };

  // ---------------------------------------------------------------- loading

  window.corduroyIndex = (index) => {
    state.arrived = index;
  };
  window.corduroyChunk = (number, events) => {
    state.chunks[number] = events;
  };

  function load(src) {
    return new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = src;
      script.onload = () => {
        script.remove();
        resolve();
      };
      script.onerror = () => {
        script.remove();
        reject(new Error(src));
      };
      document.head.append(script);
    });
  }

  const chunkName = (number) => `events-${String(number).padStart(6, '0')}.js`;

  async function poll() {
    if (state.loading) return;
    state.loading = true;
    try {
      state.arrived = null;
      await load(`index.js?t=${Date.now()}`);
      const index = state.arrived;
      if (!index || index.format !== FORMAT) {
        say('this page and the files beside it are of different versions: run the agent again');
        return;
      }
      const old = state.index;
      const again =
        !old || old.generation !== index.generation || index.chunks < old.chunks || index.last < old.last;
      if (!again && old.last === index.last && old.behind === index.behind) {
        tick();
        return;
      }
      if (again) state.chunks = [];
      // The last chunk read before may have grown since.
      const from = again ? 0 : Math.max(0, old.chunks - 1);
      for (let number = from; number < index.chunks; number += 1) {
        await load(`${chunkName(number)}?v=${index.generation}.${index.last}`);
      }
      state.chunks.length = index.chunks;
      state.index = index;
      state.events = state.chunks.flat().filter(Boolean);
      state.model = derive(state.events);
      draw();
      tick();
    } catch (error) {
      if (!state.index) say('nothing written yet: start the agent, and this page follows it');
    } finally {
      state.loading = false;
    }
  }

  // ---------------------------------------------------------------- small helpers

  /** An element with attributes and children; strings become text, never markup. */
  function h(tag, attributes, ...children) {
    const element = document.createElement(tag);
    for (const [name, value] of Object.entries(attributes || {})) {
      if (value === false || value == null) continue;
      if (name === 'class') element.className = value;
      else if (name.startsWith('on')) element.addEventListener(name.slice(2), value);
      else if (value === true) element.setAttribute(name, '');
      else element.setAttribute(name, String(value));
    }
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      element.append(child instanceof Node ? child : String(child));
    }
    return element;
  }

  /** Make `children` the whole content of `element`, leaving out what is not there. */
  function fill(element, ...children) {
    element.replaceChildren(...children.flat().filter((child) => child != null && child !== false));
  }

  const pad = (number) => String(number).padStart(2, '0');
  function clock(ms) {
    const at = new Date(ms);
    return `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`;
  }
  function day(ms) {
    const at = new Date(ms);
    return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
  }
  const when = (ms) => (day(ms) === day(Date.now()) ? clock(ms) : `${day(ms)} ${clock(ms)}`);
  function span(ms) {
    if (ms == null || Number.isNaN(ms)) return '';
    if (ms < 1000) return `${Math.round(ms)} ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)} s`;
    const minutes = Math.floor(ms / 60000);
    if (minutes < 60) return `${minutes} m ${Math.round((ms % 60000) / 1000)} s`;
    return `${Math.floor(minutes / 60)} h ${minutes % 60} m`;
  }
  function count(number) {
    if (number == null) return '–';
    if (number < 1000) return String(number);
    if (number < 1e6) return `${(number / 1000).toFixed(number < 10000 ? 1 : 0)}k`;
    return `${(number / 1e6).toFixed(2)}M`;
  }
  const oneLine = (text, most) => {
    const flat = String(text).replace(/\s+/g, ' ').trim();
    return flat.length > most ? `${flat.slice(0, most)}…` : flat;
  };
  const pretty = (value) => (typeof value === 'string' ? value : JSON.stringify(value, null, 2));
  /** A tool's input on one line: `command: ls -la  path: src`. */
  function brief(input) {
    if (input == null) return '';
    if (typeof input !== 'object' || Array.isArray(input)) return oneLine(pretty(input), 200);
    const parts = Object.entries(input).map(([key, value]) =>
      `${key}: ${typeof value === 'string' ? value : JSON.stringify(value)}`);
    return oneLine(parts.join('   '), 200);
  }
  function shown(text) {
    const whole = String(text);
    if (whole.length <= MAX_SHOWN) return whole;
    return `${whole.slice(0, MAX_SHOWN)}\n[… ${whole.length - MAX_SHOWN} more characters: the log has them]`;
  }
  const record = (payload) => (payload && typeof payload === 'object' && !Array.isArray(payload) ? payload : {});

  /** The text and the thinking of a `message` event's payload. */
  function parts(payload) {
    const list = Array.isArray(payload.parts) ? payload.parts : Array.isArray(payload.content) ? payload.content : [];
    const of = (type) => list
      .filter((part) => part && part.type === type && typeof part.text === 'string')
      .map((part) => part.text)
      .join('\n');
    const images = list.filter((part) => part && part.type === 'image').length;
    return { text: of('text'), thinking: of('thinking'), images };
  }

  // ---------------------------------------------------------------- the model of the log

  function derive(events) {
    const sessions = new Map();
    const problems = [];
    const agent = { processes: [], plugins: new Map(), models: new Map(), tools: new Map(), asked: new Map() };
    const session = (id) => {
      if (!sessions.has(id)) {
        sessions.set(id, {
          id, parent: null, task: null, items: [], calls: new Map(), run: null, request: null,
          first: null, last: null, said: '', status: 'idle', asking: 0, waiting: 0, runs: 0,
          tokensIn: 0, tokensOut: 0,
        });
      }
      return sessions.get(id);
    };
    const problem = (event, kind, text) => problems.push({ event, kind, text });
    const tally = (map, key, start) => {
      if (!map.has(key)) map.set(key, start());
      return map.get(key);
    };

    for (const event of events) {
      const payload = record(event.payload);
      const plugin = payload.plugin || event.source.replace(/^plugin:/, '');
      // What concerns the whole agent, whichever session it happened in.
      switch (event.topic) {
        case 'process/start':
          agent.processes.push({ event, ...payload });
          agent.plugins = new Map();
          break;
        case 'plugin/activated':
          agent.plugins.set(plugin, { state: 'started', event });
          break;
        case 'plugin/disposed':
          agent.plugins.set(plugin, { state: 'unloaded', event });
          break;
        case 'plugin/failed':
        case 'plugin/stopped':
        case 'plugin/trapped':
        case 'plugin/cascade': {
          const reason = payload.reason || payload.error || (typeof event.payload === 'string' ? event.payload : '');
          const word = event.topic.slice('plugin/'.length);
          if (word === 'failed' || word === 'stopped') agent.plugins.set(plugin, { state: word, reason, event });
          problem(event, `add-on ${word}`, `${plugin}${reason ? `: ${reason}` : ''}`);
          break;
        }
        case 'capability/denied':
          problem(event, 'outside a grant', `${payload.plugin} was refused ${payload.capability} on ${payload.resource}`);
          break;
        case 'llm/request':
          tally(agent.asked, `${event.source.replace(/^plugin:/, '')} → ${payload.model}`, () => ({ requests: 0, failed: 0, in: 0, out: 0 })).requests += 1;
          break;
        case 'llm/response': {
          const asked = tally(agent.asked, `${event.source.replace(/^plugin:/, '')} → ${payload.model}`, () => ({ requests: 0, failed: 0, in: 0, out: 0 }));
          asked.in += payload.input_tokens || 0;
          asked.out += payload.output_tokens || 0;
          if (payload.ok === false) {
            asked.failed += 1;
            problem(event, 'model call failed', `${event.source} asked ${payload.model}: ${oneLine(payload.output || '', 300)}`);
          }
          break;
        }
        case 'http/response':
          if (payload.error) problem(event, 'request failed', `${event.source}: ${payload.url}: ${payload.error}`);
          break;
        default:
      }
      if (event.session == null) continue;

      const s = session(event.session);
      s.first = s.first || event;
      s.last = event;
      const call = payload.id != null ? s.calls.get(payload.id) : null;
      switch (event.topic) {
        case 'session/start':
          s.parent = payload.parent || null;
          s.task = payload.task || null;
          s.items.push({ kind: 'start', event });
          break;
        case 'message': {
          const { text, thinking, images } = parts(payload);
          const user = payload.role === 'user';
          // The model's answer, which may be tool calls and no words, ends its request.
          if (!user && s.request) {
            s.request.took = event.time - s.request.event.time;
            s.request = null;
          }
          if (!text && !thinking && !images) break;
          if (user && !s.run) {
            s.run = { began: event.time, in: 0, out: 0, requests: 0, calls: 0 };
            s.runs += 1;
          }
          if (user && event.source === 'user' && !s.said) s.said = text;
          s.items.push({ kind: user ? 'user' : 'agent', event, text, thinking, images });
          break;
        }
        case 'model/request': {
          const item = { kind: 'model', event, ...payload, took: null, usage: null };
          s.request = item;
          s.lastRequest = item;
          if (s.run) s.run.requests += 1;
          s.items.push(item);
          tally(agent.models, `${payload.provider}/${payload.model}`, () => ({ requests: 0, in: 0, out: 0 })).requests += 1;
          break;
        }
        case 'usage': {
          const used = { in: payload.input_tokens || 0, out: payload.output_tokens || 0 };
          if (s.lastRequest && !s.lastRequest.usage) {
            s.lastRequest.usage = payload;
            const of = agent.models.get(`${s.lastRequest.provider}/${s.lastRequest.model}`);
            if (of) {
              of.in += used.in;
              of.out += used.out;
            }
          }
          if (s.run) {
            s.run.in += used.in;
            s.run.out += used.out;
          }
          s.tokensIn += used.in;
          s.tokensOut += used.out;
          break;
        }
        case 'tool/call': {
          const item = {
            kind: 'call', event, id: payload.id, tool: payload.tool, input: payload.input,
            gate: null, answer: null, started: null, result: null, took: null, waits: null,
          };
          s.calls.set(payload.id, item);
          s.items.push(item);
          if (s.run) s.run.calls += 1;
          tally(agent.tools, payload.tool, () => ({ calls: 0, failed: 0, ms: 0 })).calls += 1;
          break;
        }
        case 'gate/decision':
          if (call) {
            call.gate = { ...payload, event };
            if (payload.verdict === 'ask') s.asking += 1;
          }
          if (payload.verdict === 'deny') {
            problem(event, 'refused by a gate', `${payload.tool}: ${payload.gate || 'a gate'} said ${oneLine(payload.reason || 'no', 300)}`);
          }
          break;
        case 'gate/answer':
          if (call) {
            call.answer = { ...payload, waited: call.gate ? event.time - call.gate.event.time : null };
            s.asking = Math.max(0, s.asking - 1);
          }
          break;
        case 'tool/start':
          if (call) call.started = event.time;
          break;
        case 'tool/wait':
          if (call) call.waits = payload.signal;
          s.waiting += 1;
          break;
        case 'signal': {
          s.waiting = Math.max(0, s.waiting - 1);
          // What a waiting call waited for is its result.
          const waited = [...s.calls.values()].find((item) => item.waits === payload.name && !item.result);
          if (waited) {
            waited.result = { ok: true, output: typeof payload.payload === 'string' ? payload.payload : pretty(payload.payload) };
            waited.took = waited.started != null ? event.time - waited.started : null;
          } else {
            s.items.push({ kind: 'note', event, text: `signal ${payload.name}` });
          }
          break;
        }
        case 'tool/result': {
          const took = call && call.started != null ? event.time - call.started : null;
          if (call) {
            call.result = payload;
            call.took = took;
          }
          const tool = agent.tools.get(payload.tool);
          if (tool) tool.ms += took || 0;
          // A call a gate refused, or you did, did not fail: it did not run.
          const notRun = call && ((call.gate && call.gate.verdict === 'deny') || (call.answer && call.answer.allow === false));
          if (payload.ok === false && !notRun) {
            if (tool) tool.failed += 1;
            problem(event, 'tool failed', `${payload.tool}: ${oneLine(payload.output || '', 300)}`);
          }
          break;
        }
        case 'compaction':
          s.items.push({ kind: 'note', event, text: `the conversation so far was summarised${payload.by ? ` by ${payload.by}` : ''}`, more: payload.summary });
          break;
        case 'run/restart':
          s.items.push({ kind: 'note', tone: 'warn', event, text: `picked up after being cut short (attempt ${payload.attempt})` });
          problem(event, 'run cut short', `picked up again, attempt ${payload.attempt}`);
          break;
        case 'run/stop':
          s.items.push({ kind: 'note', tone: 'warn', event, text: 'asked to stop' });
          break;
        case 'turn/end': {
          const run = s.run || { began: s.first.time, in: 0, out: 0, requests: 0, calls: 0 };
          if (s.request) {
            s.request.took = event.time - s.request.event.time;
            s.request.failed = payload.status === 'failed';
            s.request = null;
          }
          s.items.push({ kind: 'end', event, status: payload.status, reason: payload.reason, run, took: event.time - run.began });
          s.status = payload.status;
          s.run = null;
          s.asking = 0;
          s.waiting = 0;
          if (payload.status === 'failed') problem(event, 'run failed', oneLine(payload.reason || 'no reason given', 400));
          if (payload.status === 'turn-limit') problem(event, 'turn limit', 'the run reached the most turns it may take');
          break;
        }
        case 'capability/denied':
          s.items.push({ kind: 'note', tone: 'warn', event, text: `${payload.plugin} was refused ${payload.capability} on ${payload.resource}` });
          break;
        case 'plugin/log':
          s.items.push({ kind: 'note', event, text: `${event.source}: ${typeof event.payload === 'string' ? event.payload : payload.message || JSON.stringify(event.payload)}` });
          break;
        default:
      }
    }

    // The questions nobody has answered yet: what a person has to do before a run goes on.
    const asks = [];
    for (const s of sessions.values()) {
      if (s.run) s.status = s.asking ? 'asking' : s.waiting ? 'waiting' : Date.now() - s.last.time < 120000 ? 'running' : 'unfinished';
      if (s.status !== 'asking') continue;
      for (const item of s.items) {
        if (item.kind === 'call' && item.gate && item.gate.verdict === 'ask' && !item.answer && !item.result) asks.push({ session: s.id, call: item });
      }
    }
    return { sessions, problems, agent, asks };
  }

  const STATUS = {
    finished: ['finished', 'good'],
    failed: ['failed', 'bad'],
    'turn-limit': ['turn limit', 'warn'],
    stopped: ['stopped', 'warn'],
    yielded: ['yielded', ''],
    asking: ['asks you', 'warn'],
    waiting: ['waiting', 'busy'],
    running: ['running', 'busy'],
    unfinished: ['unfinished', 'warn'],
    idle: ['', ''],
  };
  function badge(status) {
    const [word, tone] = STATUS[status] || [status, ''];
    return word ? h('span', { class: `badge ${tone}` }, word) : null;
  }

  // ---------------------------------------------------------------- routes

  function route() {
    const hash = decodeURIComponent(location.hash.slice(1));
    if (hash === 'problems' || hash === 'agent' || hash === 'events') return { view: hash };
    if (hash.startsWith('s/')) {
      const [id, at] = hash.slice(2).split('@');
      return { view: 'session', id, at: at ? Number(at) : null };
    }
    return { view: 'session', id: null, at: null };
  }
  const link = (id, at) => `#${encodeURIComponent(`s/${id}${at ? `@${at}` : ''}`)}`;

  /** The session shown when none is named: the one a person's prompt last went to. */
  function newest(sessions) {
    let found = null;
    for (const s of sessions.values()) {
      if (!found || s.last.id > found.last.id) found = s;
    }
    while (found && found.parent && sessions.has(found.parent)) found = sessions.get(found.parent);
    return found;
  }

  // ---------------------------------------------------------------- drawing

  function say(text, live) {
    const status = document.getElementById('status');
    status.textContent = text;
    status.className = `status${live ? ' live' : ''}`;
  }

  function tick() {
    const { index, events } = state;
    if (!index) return;
    const last = events[events.length - 1];
    if (!last) {
      say('the log is empty so far', true);
    } else if (index.behind) {
      say(`catching up with a long log: ${index.events} events so far`, true);
    } else {
      say(`${index.events} events · last at ${when(last.time)}`, true);
    }
  }

  /** The bar over every view while a gate's question waits: where to answer it. */
  function drawAsks() {
    const bar = document.getElementById('asks');
    const { asks } = state.model;
    bar.hidden = !asks.length;
    fill(bar, asks.map(({ session, call }) => h('div', { class: 'ask' },
      h('b', null, `${session} waits for your answer`),
      h('span', { class: 'what' }, `${call.gate.gate || 'a gate'} asks about `, h('code', null, call.tool), ` ${brief(call.input)}`),
      h('span', null, 'Answer in the terminal where corduroy runs, or with ', h('code', null, 'corduroy continue'), '.'),
      h('a', { href: link(session, call.event.id) }, 'show it'))));
  }

  function keep(key, details) {
    if (state.open.has(key)) details.open = true;
    details.addEventListener('toggle', () => {
      if (details.open) state.open.add(key);
      else state.open.delete(key);
    });
    return details;
  }

  function drawNav(current) {
    const { sessions, problems } = state.model;
    const nav = document.getElementById('nav');
    const children = new Map();
    const roots = [];
    for (const s of sessions.values()) {
      if (s.parent && sessions.has(s.parent)) {
        if (!children.has(s.parent)) children.set(s.parent, []);
        children.get(s.parent).push(s);
      } else {
        roots.push(s);
      }
    }
    roots.sort((a, b) => b.last.id - a.last.id);
    const entry = (s, depth) => [
      h('a', { href: link(s.id), class: `${current.id === s.id ? 'here' : ''}${depth ? ' child' : ''}`, title: s.id },
        h('div', { class: 'row' }, h('span', { class: 'name grow' }, s.id), badge(s.status)),
        h('div', { class: 'said' }, oneLine(s.said || s.task || 'nothing said yet', 80))),
      ...(children.get(s.id) || []).flatMap((child) => entry(child, depth + 1)),
    ];
    const view = (name, label, extra) =>
      h('a', { href: `#${name}`, class: current.view === name ? 'here' : '' }, h('div', { class: 'row' }, h('span', { class: 'grow' }, label), extra));
    fill(nav, 
      h('h2', null, 'Sessions'),
      ...(roots.length ? roots.flatMap((s) => entry(s, 0)) : [h('div', { class: 'said none' }, 'none yet')]),
      h('h2', null, 'The agent'),
      view('problems', 'Problems', problems.length ? h('span', { class: 'badge bad' }, problems.length) : null),
      view('agent', 'What it has and used'),
      view('events', 'Every event'),
    );
  }

  function more(key, total) {
    const extra = state.extra[key] || 0;
    const hidden = Math.max(0, total - WINDOW - extra);
    const button = hidden
      ? h('button', { class: 'more', onclick: () => { state.extra[key] = extra + WINDOW; draw(); } },
        `show ${Math.min(hidden, WINDOW)} earlier (${hidden} not shown)`)
      : null;
    return { from: hidden, button };
  }

  function drawCall(s, item) {
    const { gate, answer, result } = item;
    const refused = gate && gate.verdict === 'deny';
    const declined = answer && answer.allow === false;
    const asking = gate && gate.verdict === 'ask' && !answer && !result;
    let mark = ['…', '', 'still running, or never ran'];
    if (result) mark = result.ok ? ['✓', 'good', 'returned'] : ['✗', 'bad', 'failed'];
    if (refused || declined) mark = ['⊘', 'bad', 'not run'];
    else if (asking) mark = ['?', 'warn', 'waits for your answer in the terminal'];
    const tone = refused || declined || (result && !result.ok) ? 'bad' : asking ? 'warn' : '';
    const body = [h('div', { class: 'label' }, 'Input'), h('pre', null, shown(pretty(item.input)))];
    if (gate && gate.verdict !== 'allow') {
      const by = gate.gate || 'a gate';
      body.push(h('div', { class: `gate ${refused ? 'bad' : 'warn'}` },
        refused ? `Refused by ${by}: ${gate.reason || ''}` : `${by} asked you: ${gate.reason || ''}`));
    }
    if (answer) {
      body.push(h('div', { class: `gate ${answer.allow ? 'good' : 'bad'}` },
        `You ${answer.allow ? 'allowed it' : 'refused it'}`,
        answer.waited != null ? ` after ${span(answer.waited)}` : '',
        !answer.allow && answer.reason ? `: ${answer.reason}` : ''));
    }
    if (item.waits) {
      // A call that started a session waits for that session's end.
      const started = (/^session\/(.+)\/end$/.exec(item.waits) || [])[1];
      body.push(h('div', { class: 'gate good' }, started && state.model.sessions.has(started)
        ? ['It started the session ', h('a', { href: link(started) }, started), result ? ', whose answer is its result' : ' and waits for its answer']
        : `Its result comes with the signal ${item.waits}`));
    }
    if (result) {
      body.push(h('div', { class: 'label' }, result.ok ? 'Result' : 'Error'));
      body.push(h('pre', null, shown(result.output == null ? pretty(result.content) : result.output) || '(nothing)'));
      const images = Array.isArray(result.content) ? result.content.filter((block) => block && block.type === 'image').length : 0;
      if (images) body.push(h('div', { class: 'quiet' }, `and ${images} image(s), which the log holds`));
    }
    const details = h('details', { class: `call item ${tone}`, id: `e${item.event.id}` },
      h('summary', null,
        h('span', { class: `mark ${mark[1]}`, title: mark[2] }, mark[0]),
        h('span', { class: 'tool' }, item.tool),
        h('span', { class: 'input' }, brief(item.input)),
        h('span', { class: 'took' }, item.took != null ? span(item.took) : when(item.event.time))),
      h('div', { class: 'body' }, body));
    if (asking) details.open = true;
    return keep(`call:${s.id}:${item.event.id}`, details);
  }

  function drawItem(s, item) {
    const { event } = item;
    switch (item.kind) {
      case 'start':
        return h('div', { class: 'meta', id: `e${event.id}` }, `session started ${when(event.time)}`);
      case 'user':
      case 'agent': {
        const user = item.kind === 'user';
        const who = user ? (event.source === 'user' ? 'You' : `To the model, from ${event.source}`) : 'Agent';
        return h('div', { class: 'item', id: `e${event.id}` },
          h('div', { class: 'who' }, h('b', null, who), h('span', null, when(event.time))),
          item.thinking
            ? keep(`think:${event.id}`, h('details', { class: 'thinking' }, h('summary', null, 'thinking'), h('pre', null, shown(item.thinking))))
            : null,
          item.text ? h('div', { class: user ? 'said-user' : 'said-agent' }, shown(item.text)) : null,
          item.images ? h('div', { class: 'quiet' }, `${item.images} image(s), which the log holds`) : null);
      }
      case 'call':
        return drawCall(s, item);
      case 'model': {
        const usage = item.usage
          ? ` · ${count(item.usage.input_tokens)} in, ${count(item.usage.output_tokens)} out${item.usage.stop && item.usage.stop !== 'finished' && item.usage.stop !== 'tool_calls' ? ` · stopped: ${item.usage.stop}` : ''}`
          : '';
        const took = item.took != null ? ` · ${span(item.took)}` : s.request === item ? ' · waiting for it' : '';
        return h('div', { class: `meta${item.failed ? ' bad' : ''}`, id: `e${event.id}` },
          `asked ${item.provider}/${item.model} (${item.messages} messages, ${item.tools} tools)${took}${usage}`);
      }
      case 'end': {
        const tone = item.status === 'failed' ? 'bad' : item.status === 'finished' ? '' : 'warn';
        return h('div', { class: `end ${tone}`, id: `e${event.id}` },
          badge(item.status),
          h('span', null, `in ${span(item.took)}`),
          h('span', { class: 'quiet' }, `${item.run.requests} model requests · ${item.run.calls} tool calls · ${count(item.run.in)} tokens in, ${count(item.run.out)} out`),
          item.reason ? h('div', { class: 'reason' }, shown(item.reason)) : null);
      }
      default: {
        const line = h('div', { class: `meta ${item.tone || ''}`, id: `e${event.id}` }, item.text);
        if (!item.more) return line;
        return h('div', null, line, keep(`more:${event.id}`, h('details', { class: 'note item' }, h('summary', null, 'the summary'), h('div', { class: 'body' }, h('pre', null, shown(item.more))))));
      }
    }
  }

  function drawSession(main, current) {
    const { sessions } = state.model;
    const s = current.id ? sessions.get(current.id) : newest(sessions);
    if (!s) {
      fill(main, h('p', { class: 'empty' }, current.id
        ? `The log has no session named ${current.id}.`
        : 'No session has run yet. Give the agent a prompt (corduroy run "…") and it shows here as it goes.'));
      return { id: null };
    }
    const { from, button } = more(`s:${s.id}`, s.items.length);
    const facts = [
      s.parent ? ['started by ', h('a', { href: link(s.parent) }, s.parent)] : null,
      `${s.runs} run${s.runs === 1 ? '' : 's'}`,
      `${count(s.tokensIn)} tokens in, ${count(s.tokensOut)} out`,
      s.first ? `since ${when(s.first.time)}` : null,
    ].filter(Boolean);
    fill(main, 
      h('h1', null, h('span', null, s.id), ' ', badge(s.status)),
      h('p', { class: 'quiet' }, ...facts.flatMap((fact, at) => (at ? [' · ', fact] : [fact]))),
      s.task ? h('div', { class: 'item' }, h('div', { class: 'who' }, h('b', null, 'Its task')), h('div', { class: 'said-user' }, shown(s.task))) : null,
      button,
      ...s.items.slice(from).map((item) => drawItem(s, item)),
      s.status === 'asking' ? h('div', { class: 'end warn' }, 'A gate asks you about the call above, and the run waits. This page only shows: answer in the terminal where corduroy runs, or run ', h('code', null, 'corduroy continue'), ' in this workspace to be asked again.') : null,
      s.status === 'unfinished' ? h('div', { class: 'end warn' }, 'This run was cut short. ', h('code', null, 'corduroy continue'), ' picks it up; ', h('code', null, 'corduroy stop'), ' ends it.') : null,
    );
    return { id: s.id };
  }

  function drawProblems(main) {
    const { problems } = state.model;
    if (!problems.length) {
      fill(main, h('h1', null, 'Problems'), h('p', { class: 'empty' }, 'Nothing has gone wrong: no failed run, tool or add-on, and nothing refused.'));
      return;
    }
    const { from, button } = more('problems', problems.length);
    const kinds = new Map();
    for (const { kind } of problems) kinds.set(kind, (kinds.get(kind) || 0) + 1);
    fill(main, 
      h('h1', null, 'Problems'),
      h('p', { class: 'quiet' }, [...kinds].map(([kind, number]) => `${number} × ${kind}`).join(' · ')),
      button,
      h('table', null,
        h('thead', null, h('tr', null, h('th', null, 'When'), h('th', null, 'Session'), h('th', null, 'What'), h('th', null, 'Said'))),
        h('tbody', null, problems.slice(from).reverse().map(({ event, kind, text }) =>
          h('tr', null,
            h('td', { class: 'nowrap quiet' }, when(event.time)),
            h('td', { class: 'nowrap' }, event.session ? h('a', { href: link(event.session, event.cause || event.id) }, event.session) : h('span', { class: 'quiet' }, '–')),
            h('td', { class: 'nowrap' }, h('span', { class: 'badge bad' }, kind)),
            h('td', null, text))))),
    );
  }

  function table(headings, rows) {
    return h('table', null,
      h('thead', null, h('tr', null, headings.map((heading) => h('th', { class: heading.startsWith('#') ? 'num' : '' }, heading.replace(/^#/, ''))))),
      h('tbody', null, rows.map((row) => h('tr', null, row.map((cell, at) => h('td', { class: headings[at].startsWith('#') ? 'num' : '' }, cell))))));
  }

  function drawAgent(main) {
    const { agent, sessions } = state.model;
    const process = agent.processes[agent.processes.length - 1];
    const plugins = [...agent.plugins].sort(([a], [b]) => a.localeCompare(b));
    const tone = { started: 'good', failed: 'bad', stopped: 'bad', unloaded: '' };
    const totals = [...sessions.values()].reduce((sum, s) => ({ in: sum.in + s.tokensIn, out: sum.out + s.tokensOut }), { in: 0, out: 0 });
    fill(main, 
      h('h1', null, 'What the agent has and used'),
      h('p', { class: 'quiet' }, process
        ? `Last started ${when(process.event.time)} as ${process.program} ${process.version} on ${process.platform}${process.world ? `, hosting add-ons of ${process.world}` : ''}. Started ${agent.processes.length} time(s) in this log.`
        : 'The log does not say what wrote it.'),
      h('p', { class: 'quiet' }, 'This is what the record shows. What the agent is granted is in its agent file, which no add-on can read: ', h('code', null, 'corduroy check'), ' lists it.'),
      h('h3', null, `Plugins, as of the last start (${plugins.length})`),
      table(['Name', 'State', 'Why'], plugins.map(([name, plugin]) =>
        [h('span', { class: 'mono' }, name), h('span', { class: `badge ${tone[plugin.state] || ''}` }, plugin.state), plugin.reason || ''])),
      h('h3', null, `Models the loop asked (${count(totals.in)} tokens in, ${count(totals.out)} out, over every session)`),
      table(['Model', '#Requests', '#Tokens in', '#Tokens out'], [...agent.models].map(([name, model]) =>
        [h('span', { class: 'mono' }, name), model.requests, count(model.in), count(model.out)])),
      agent.asked.size ? h('h3', null, 'Models add-ons asked') : null,
      agent.asked.size ? table(['Add-on → model', '#Requests', '#Failed', '#Tokens in', '#Tokens out'], [...agent.asked].map(([name, asked]) =>
        [h('span', { class: 'mono' }, name), asked.requests, asked.failed, count(asked.in), count(asked.out)])) : null,
      h('h3', null, 'Tools called'),
      table(['Tool', '#Calls', '#Failed', '#Time in all'], [...agent.tools].sort(([, a], [, b]) => b.calls - a.calls).map(([name, tool]) =>
        [h('span', { class: 'mono' }, name), tool.calls, tool.failed || '', span(tool.ms)])),
    );
  }

  function drawEvents(main) {
    const words = state.filter.toLowerCase().split(/\s+/).filter(Boolean);
    const texts = (event) => `${event.topic} ${event.session || ''} ${event.source} #${event.id} ${typeof event.payload === 'string' ? event.payload : JSON.stringify(event.payload)}`.toLowerCase();
    const matching = words.length ? state.events.filter((event) => { const text = texts(event); return words.every((word) => text.includes(word)); }) : state.events;
    const { from, button } = more('events', matching.length);
    const rows = matching.slice(from).reverse().flatMap((event) => {
      const key = `event:${event.id}`;
      const opened = state.open.has(key);
      const row = h('tr', { class: `pick${opened ? ' opened' : ''}`, onclick: () => { if (opened) state.open.delete(key); else state.open.add(key); draw(); } },
        h('td', { class: 'nowrap quiet' }, when(event.time)),
        h('td', { class: 'num quiet' }, `#${event.id}`),
        h('td', { class: 'nowrap' }, event.session ? h('a', { href: link(event.session, event.id), onclick: (click) => click.stopPropagation() }, event.session) : ''),
        h('td', { class: 'nowrap quiet' }, event.source),
        h('td', { class: 'nowrap mono' }, event.topic),
        h('td', { class: 'clip mono quiet' }, oneLine(typeof event.payload === 'string' ? event.payload : JSON.stringify(event.payload), 240)));
      if (!opened) return [row];
      const note = [event.cause ? `after #${event.cause}` : null, event.cut ? `cut: the log has all ${event.cut} bytes` : null].filter(Boolean).join(' · ');
      return [row, h('tr', null, h('td', { colspan: 6 }, note ? h('div', { class: 'quiet' }, note) : null, h('pre', null, shown(pretty(event.payload)))))];
    });
    const input = h('input', {
      type: 'search', placeholder: 'filter: a topic, a session, a word in the payload', value: state.filter, 'aria-label': 'Filter events',
      oninput: (typed) => { state.filter = typed.target.value; state.refocus = typed.target.selectionStart; draw(); },
    });
    fill(main, 
      h('h1', null, 'Every event'),
      h('p', { class: 'quiet' }, 'The log as it is, newest first. Click a line for its whole payload.'),
      h('div', { class: 'bar' }, input, h('span', { class: 'quiet' }, `${matching.length} of ${state.events.length}`)),
      button,
      h('table', null, h('tbody', null, rows)),
    );
    if (state.refocus != null) {
      input.focus();
      input.setSelectionRange(state.refocus, state.refocus);
      state.refocus = null;
    }
  }

  function draw() {
    if (!state.model) return;
    const current = route();
    const main = document.getElementById('main');
    const view = `${current.view}:${current.id || ''}`;
    const sameView = state.drawn === view;
    const atEnd = main.scrollHeight - main.scrollTop - main.clientHeight < 80;
    const top = main.scrollTop;
    main.className = current.view === 'events' ? 'wide' : '';
    let shownSession = null;
    if (current.view === 'problems') drawProblems(main);
    else if (current.view === 'agent') drawAgent(main);
    else if (current.view === 'events') drawEvents(main);
    else shownSession = drawSession(main, current).id;
    drawNav({ view: current.view, id: shownSession });
    drawAsks();
    const process = state.model.agent.processes[state.model.agent.processes.length - 1];
    document.getElementById('build').textContent = process ? `${process.version} · ${process.platform}` : '';
    // In the tab's name too, for when the page is behind another.
    const waits = state.model.asks.length ? '(asks you) ' : '';
    document.title = `${waits}${shownSession ? `${shownSession} · ` : ''}corduroy`;
    state.drawn = view;

    const target = current.at && state.jumped !== location.hash ? document.getElementById(`e${current.at}`) : null;
    if (target) {
      state.jumped = location.hash;
      if (target.tagName === 'DETAILS') target.open = true;
      target.scrollIntoView({ block: 'center' });
      target.classList.add('flash');
    } else if (!sameView) {
      // A session is read from where it is now; a list from its top.
      main.scrollTop = current.view === 'session' ? main.scrollHeight : 0;
    } else if (current.view === 'session' && atEnd) {
      main.scrollTop = main.scrollHeight;
    } else {
      main.scrollTop = top;
    }
  }

  window.addEventListener('hashchange', () => {
    state.jumped = null;
    draw();
  });
  setInterval(poll, POLL_MS);
  poll();
})();
