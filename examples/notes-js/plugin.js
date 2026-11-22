// A notes plugin in JavaScript: the model keeps short notes that outlive the process.
//
// Built with: corduroy-js build examples/notes-js/plugin.js --out notes_js.wasm \
//               --capabilities state,log,clock,random
//
// Notes live in the plugin's `state` store, so they survive a restart of the plugin and of
// the harness. `console.log`, `new Date()` and `Math.random()` work as usual: the SDK routes
// them to the `log`, `clock` and `random` capabilities.

import { get, keys, remove, set } from 'corduroy:plugin/state@0.1.0';
import { ToolError, definePlugin } from 'corduroy-plugin';

const PREFIX = 'note/';

export default definePlugin({
  name: 'notes-js',
  version: '0.1.0',

  tools: {
    note_add: {
      description: 'Save a short note for later. Returns its id.',
      input: {
        type: 'object',
        properties: { text: { type: 'string' } },
        required: ['text'],
      },
      run({ text }) {
        if (typeof text !== 'string' || text.trim() === '') {
          throw new ToolError('invalid-input', 'text must be a non-empty string');
        }
        const id = Math.floor(Math.random() * 0xffffffff).toString(16).padStart(8, '0');
        set(PREFIX + id, JSON.stringify({ text, at: new Date().toISOString() }));
        console.log(`saved note ${id}`);
        return id;
      },
    },

    note_list: {
      description: 'List every saved note, oldest first.',
      readOnly: true,
      replaySafe: true,
      run() {
        return keys()
          .filter((key) => key.startsWith(PREFIX))
          .map((key) => ({ id: key.slice(PREFIX.length), ...JSON.parse(get(key)) }))
          .sort((a, b) => a.at.localeCompare(b.at));
      },
    },

    note_remove: {
      description: 'Remove a note by id.',
      input: {
        type: 'object',
        properties: { id: { type: 'string' } },
        required: ['id'],
      },
      run({ id }) {
        if (get(PREFIX + id) === undefined) {
          throw new ToolError('invalid-input', `no note ${id}`);
        }
        remove(PREFIX + id);
        return `removed ${id}`;
      },
    },
  },

  prompt() {
    const count = keys().filter((key) => key.startsWith(PREFIX)).length;
    return count === 0 ? undefined : `You have ${count} saved note(s); note_list shows them.`;
  },
});
