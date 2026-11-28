/*
 * corduroy.h: extensions for the corduroy harness, through C.
 *
 * A PROTOTYPE. Nothing here is promised: see G23 in docs/open-questions.md and
 * docs/c-extensions.md. It mirrors `src/abi.rs` by hand and must be changed with it.
 *
 * The rules of the boundary:
 *
 * 1. Nothing is freed across it. A `corduroy_str` is borrowed for the length of the call it
 *    is passed to, whichever way it goes. Whoever produces text hands it to a function of
 *    whoever wants it, which copies: an extension answers by `host->write(out, text)`, and
 *    reads the host's answer through a `corduroy_receive_fn` of its own.
 * 2. The extension calls the host only through the `corduroy_host` table it is handed. The
 *    table lasts as long as the program, so it may be kept. A library built against this
 *    header has no undefined symbols, and may be linked in or loaded.
 * 3. A `corduroy_ctx` is valid inside `apply` only. Everything registered through it
 *    belongs to the extension and is taken back when it is unloaded.
 * 4. Every callback may be called from any thread, and from several at once. `user` is the
 *    extension's own pointer, handed back untouched; what it points to is the extension's
 *    to keep safe.
 * 5. A callback returns; it does not unwind (no `longjmp`, no C++ exception) into the host.
 * 6. Every table starts with its own size, and grows only at its end. Before using a field
 *    newer than the first version, check that `size` reaches it.
 */
#ifndef CORDUROY_H
#define CORDUROY_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The version of this interface. An entry function refuses a host that speaks another. */
#define CORDUROY_ABI 0u

/* Text: UTF-8, not NUL-terminated, borrowed for the call. `ptr` may be NULL when `len` is 0. */
typedef struct corduroy_str {
    const uint8_t *ptr;
    size_t len;
} corduroy_str;

/* A string literal as a corduroy_str. */
#define CORDUROY_STR(literal) ((corduroy_str){(const uint8_t *)(literal), sizeof(literal) - 1})

/* How a call went. What was written to `out` is the answer, or why not. */
enum {
    CORDUROY_OK = 0,
    CORDUROY_INVALID_INPUT = 1,
    CORDUROY_FAILED = 2,
    CORDUROY_DENIED = 3,
    CORDUROY_UNKNOWN = 4,
    /* Nothing to say: a prompt part with no text this time. */
    CORDUROY_NONE = 5
};

/* A gate's answer. What was written to `reason` is shown to the person or the model. */
enum {
    CORDUROY_NO_OPINION = 0,
    CORDUROY_ALLOW = 1,
    CORDUROY_ASK = 2,
    CORDUROY_DENY = 3
};

/* What a tool says of itself, in `corduroy_tool.flags`. */
#define CORDUROY_TOOL_REPLAY_SAFE 1u
#define CORDUROY_TOOL_READ_ONLY 2u

/* Where a callback writes its answer. */
typedef struct corduroy_out corduroy_out;
/* The extension's context, inside `apply`. */
typedef struct corduroy_ctx corduroy_ctx;
/* The extension's hold on the harness after `apply`, until its `drop` is called. */
typedef struct corduroy_handle corduroy_handle;
/* An answer still owed. Completed exactly once, from any thread. */
typedef struct corduroy_completion corduroy_completion;

typedef int32_t (*corduroy_tool_fn)(void *user, corduroy_str input, corduroy_out *out);
typedef int32_t (*corduroy_service_fn)(void *user, corduroy_str caller, corduroy_str method,
                                       corduroy_str payload, corduroy_out *out);
typedef int32_t (*corduroy_gate_fn)(void *user, corduroy_str point, corduroy_str request,
                                    corduroy_out *reason);
/* A prompt part, or a secret's value. */
typedef int32_t (*corduroy_text_fn)(void *user, corduroy_out *out);
typedef void (*corduroy_listen_fn)(void *user, corduroy_str topic, corduroy_str source,
                                   corduroy_str payload);
typedef void (*corduroy_dispose_fn)(void *user);
/* How the extension is handed text the host produced. */
typedef void (*corduroy_receive_fn)(void *into, corduroy_str text);
/* Start answering `request` (JSON) and return. `done` is completed later, or before
 * returning, through `host->complete`. `request` is gone when this returns: copy what is
 * needed. */
typedef void (*corduroy_complete_fn)(void *user, corduroy_str request,
                                     corduroy_completion *done);

typedef struct corduroy_tool {
    uint32_t size; /* sizeof(corduroy_tool) */
    uint32_t flags;
    corduroy_str name;
    corduroy_str description;
    corduroy_str input_schema; /* JSON Schema, as JSON text */
    void *user;
    corduroy_tool_fn call;
} corduroy_tool;

typedef struct corduroy_provider {
    uint32_t size; /* sizeof(corduroy_provider) */
    uint32_t reserved;
    void *user;
    corduroy_complete_fn complete;
} corduroy_provider;

/* What the host offers. Functions that take a `corduroy_ctx` register something for the
 * extension and return CORDUROY_OK, or another status once the host has noted why; `apply`
 * then only has to return that status. */
typedef struct corduroy_host {
    uint32_t abi;
    uint32_t size;

    /* Anywhere. */
    void (*write)(corduroy_out *out, corduroy_str text);
    /* Whether the run this thread serves was asked to stop: 1, or 0. */
    int32_t (*stopping)(void);

    /* Inside apply. */
    void (*fail)(corduroy_ctx *ctx, corduroy_str why);
    int32_t (*tool)(corduroy_ctx *ctx, const corduroy_tool *tool);
    int32_t (*service)(corduroy_ctx *ctx, corduroy_str name, corduroy_service_fn call,
                       void *user);
    int32_t (*gate)(corduroy_ctx *ctx, corduroy_str name, corduroy_gate_fn check, void *user);
    int32_t (*prompt)(corduroy_ctx *ctx, corduroy_str name, corduroy_text_fn render,
                      void *user);
    int32_t (*secret)(corduroy_ctx *ctx, corduroy_str name, corduroy_text_fn reveal,
                      void *user);
    int32_t (*on)(corduroy_ctx *ctx, corduroy_str topics, corduroy_listen_fn listener,
                  void *user);
    /* `dispose(user)` is called when the extension is unloaded, last registered first. */
    int32_t (*effect)(corduroy_ctx *ctx, corduroy_dispose_fn dispose, void *user);
    const corduroy_handle *(*handle)(corduroy_ctx *ctx);

    /* After apply, until drop. */
    int32_t (*emit)(const corduroy_handle *handle, corduroy_str topic, corduroy_str payload);
    /* Call another plugin's service. The answer, or why not, goes to `receive(into, ..)`
     * before this returns. Needs "services" in the extension's `inject`. */
    int32_t (*call)(const corduroy_handle *handle, corduroy_str service, corduroy_str method,
                    corduroy_str payload, corduroy_receive_fn receive, void *into);

    /* The loop's side. NULL in a host built without it. */
    int32_t (*provider)(corduroy_ctx *ctx, corduroy_str name, const corduroy_provider *provider);
    /* `text` is the reply as JSON for CORDUROY_OK, and why not otherwise. */
    void (*complete)(corduroy_completion *done, int32_t status, corduroy_str text);
} corduroy_host;

/* An extension, as its entry function describes it. The host sets `abi` and `size` before
 * the call; the entry function fills in the rest. `name` and `inject` are copied before the
 * entry function's caller goes on, so they need not outlive it. */
typedef struct corduroy_extension {
    uint32_t abi;
    uint32_t size;
    corduroy_str name;
    /* The services `apply` uses: "tools", "gates", "prompts", "secrets", "services". */
    const corduroy_str *inject;
    size_t inject_len;
    void *user;
    /* Called each time the extension is activated. Not CORDUROY_OK: it is rolled back. */
    int32_t (*apply)(void *user, const corduroy_host *host, corduroy_ctx *ctx);
    /* Called once, when the host lets go of the extension. May be NULL. */
    corduroy_dispose_fn drop;
} corduroy_extension;

/* An extension exports one function of this type. By convention it is named
 * `corduroy_extension_<name>`. It returns CORDUROY_OK, or CORDUROY_FAILED for a host it
 * cannot speak to. */
typedef int32_t (*corduroy_entry_fn)(corduroy_extension *extension);

#ifdef __cplusplus
}
#endif

#endif /* CORDUROY_H */
