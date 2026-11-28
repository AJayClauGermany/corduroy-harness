/*
 * notes: an extension for the corduroy harness, in C.
 *
 * It keeps a count of notes and contributes one of everything the interface carries: a
 * tool, a service, a gate, a part of the system prompt, a secret, a listener, an effect,
 * and a model provider that answers from a thread of its own.
 *
 * It includes one header and links against nothing of the harness: every call into the
 * host goes through the table `apply` is handed.
 */
#define _POSIX_C_SOURCE 200809L

#include "corduroy.h"

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef _WIN32
#include <pthread.h>
#include <time.h>
#endif

typedef struct notes {
    const corduroy_host *host;
    const corduroy_handle *handle;
    atomic_int count;    /* notes taken */
    atomic_int heard;    /* tool results heard */
    atomic_int requests; /* requests the model was asked */
} notes;

static void write_number(const corduroy_host *host, corduroy_out *out, const char *before,
                         int number, const char *after) {
    char text[96];
    int len = snprintf(text, sizeof text, "%s%d%s", before, number, after);
    if (len > 0) {
        host->write(out, (corduroy_str){(const uint8_t *)text, (size_t)len});
    }
}

/* Whether `text` holds `word`. Text that crosses is not NUL-terminated. */
static int holds(corduroy_str text, const char *word) {
    size_t len = strlen(word);
    for (size_t at = 0; at + len <= text.len; at++) {
        if (memcmp(text.ptr + at, word, len) == 0) {
            return 1;
        }
    }
    return 0;
}

static int is(corduroy_str text, const char *word) {
    return text.len == strlen(word) && memcmp(text.ptr, word, text.len) == 0;
}

/* The tool `note`. */
static int32_t note(void *user, corduroy_str input, corduroy_out *out) {
    notes *self = user;
    if (!holds(input, "\"text\"")) {
        self->host->write(out, CORDUROY_STR("a note needs `text`"));
        return CORDUROY_INVALID_INPUT;
    }
    write_number(self->host, out, "noted (", atomic_fetch_add(&self->count, 1) + 1, ")");
    return CORDUROY_OK;
}

/* The service `notes`: `count`, and `heard`. */
static int32_t serve(void *user, corduroy_str caller, corduroy_str method, corduroy_str payload,
                     corduroy_out *out) {
    notes *self = user;
    (void)caller;
    (void)payload;
    if (is(method, "count")) {
        write_number(self->host, out, "", atomic_load(&self->count), "");
        return CORDUROY_OK;
    }
    if (is(method, "heard")) {
        write_number(self->host, out, "", atomic_load(&self->heard), "");
        return CORDUROY_OK;
    }
    self->host->write(out, CORDUROY_STR("the notes service has `count` and `heard`"));
    return CORDUROY_UNKNOWN;
}

/* The gate `notes-gate`: no note may hold the word "forbidden". */
static int32_t check(void *user, corduroy_str point, corduroy_str request, corduroy_out *reason) {
    notes *self = user;
    if (!is(point, "tool/call") || !holds(request, "forbidden")) {
        return CORDUROY_NO_OPINION;
    }
    self->host->write(reason, CORDUROY_STR("that word is not to be written down"));
    return CORDUROY_DENY;
}

/* The prompt part `notes`: nothing until there is a note. */
static int32_t render(void *user, corduroy_out *out) {
    notes *self = user;
    int count = atomic_load(&self->count);
    if (count == 0) {
        return CORDUROY_NONE;
    }
    write_number(self->host, out, "You have taken ", count, " notes.");
    return CORDUROY_OK;
}

/* The secret `notes-key`. */
static int32_t reveal(void *user, corduroy_out *out) {
    notes *self = user;
    self->host->write(out, CORDUROY_STR("s3cret"));
    return CORDUROY_OK;
}

/* A listener on `tool/result`. */
static void hear(void *user, corduroy_str topic, corduroy_str source, corduroy_str payload) {
    notes *self = user;
    (void)topic;
    (void)source;
    (void)payload;
    atomic_fetch_add(&self->heard, 1);
}

/* An effect's undoing: say so, through the handle. */
static void closed(void *user) {
    notes *self = user;
    self->host->emit(self->handle, CORDUROY_STR("notes/closed"), CORDUROY_STR("{}"));
}

/* The model `c-model`: asks for a note first, then says how many there are. */
typedef struct owed {
    notes *self;
    corduroy_completion *done;
    int request;
} owed;

static void answer(const owed *owed) {
    const corduroy_host *host = owed->self->host;
    if (owed->request == 0) {
        host->complete(
            owed->done, CORDUROY_OK,
            CORDUROY_STR("{\"message\":{\"role\":\"assistant\",\"parts\":[{\"type\":\"tool_call\","
                         "\"id\":\"c1\",\"name\":\"note\",\"input\":{\"text\":\"from C\"}}]},"
                         "\"usage\":{\"input_tokens\":7,\"output_tokens\":3},"
                         "\"stop\":\"tool_calls\"}"));
        return;
    }
    char reply[160];
    int len = snprintf(reply, sizeof reply,
                       "{\"message\":{\"role\":\"assistant\",\"parts\":[{\"type\":\"text\","
                       "\"text\":\"there are %d notes\"}]}}",
                       atomic_load(&owed->self->count));
    host->complete(owed->done, len > 0 ? CORDUROY_OK : CORDUROY_FAILED,
                   (corduroy_str){(const uint8_t *)reply, len > 0 ? (size_t)len : 0});
}

#ifndef _WIN32
static void *later(void *arg) {
    struct timespec pause = {0, 2000000};
    nanosleep(&pause, NULL);
    answer(arg);
    free(arg);
    return NULL;
}
#endif

static void complete(void *user, corduroy_str request, corduroy_completion *done) {
    notes *self = user;
    int turn = atomic_fetch_add(&self->requests, 1);
    /* The request is gone once this returns, so what the answer needs is taken now. */
    if (!holds(request, "\"messages\"")) {
        self->host->complete(done, CORDUROY_INVALID_INPUT,
                             CORDUROY_STR("a request has no messages"));
        return;
    }
#ifndef _WIN32
    /* Answer from another thread, after this call has returned. */
    owed *slot = malloc(sizeof *slot);
    pthread_t thread;
    if (slot != NULL) {
        *slot = (owed){self, done, turn};
        if (pthread_create(&thread, NULL, later, slot) == 0) {
            pthread_detach(thread);
            return;
        }
        free(slot);
    }
#endif
    /* A completion may also be completed before returning. */
    answer(&(owed){self, done, turn});
}

static int32_t apply(void *user, const corduroy_host *host, corduroy_ctx *ctx) {
    notes *self = user;
    self->host = host;
    self->handle = host->handle(ctx);

    corduroy_tool tool = {
        .size = sizeof tool,
        .name = CORDUROY_STR("note"),
        .description = CORDUROY_STR("Write a note down."),
        .input_schema = CORDUROY_STR(
            "{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}"),
        .user = self,
        .call = note,
    };
    /* A registration that fails has told the host why; returning the status is enough. */
    int32_t status = host->tool(ctx, &tool);
    if (status == CORDUROY_OK) status = host->service(ctx, CORDUROY_STR("notes"), serve, self);
    if (status == CORDUROY_OK) status = host->gate(ctx, CORDUROY_STR("notes-gate"), check, self);
    if (status == CORDUROY_OK) status = host->prompt(ctx, CORDUROY_STR("notes"), render, self);
    if (status == CORDUROY_OK) status = host->secret(ctx, CORDUROY_STR("notes-key"), reveal, self);
    if (status == CORDUROY_OK) status = host->on(ctx, CORDUROY_STR("tool/result"), hear, self);
    if (status == CORDUROY_OK) status = host->effect(ctx, closed, self);
    /* Newer than the first table, and absent from a host built without the loop. */
    if (status == CORDUROY_OK && host->size >= sizeof(corduroy_host) && host->provider != NULL) {
        corduroy_provider provider = {.size = sizeof provider, .user = self, .complete = complete};
        status = host->provider(ctx, CORDUROY_STR("c-model"), &provider);
    }
    return status;
}

static const corduroy_str INJECT[] = {
    {(const uint8_t *)"tools", 5},   {(const uint8_t *)"services", 8},
    {(const uint8_t *)"gates", 5},   {(const uint8_t *)"prompts", 7},
    {(const uint8_t *)"secrets", 7},
};

int32_t corduroy_extension_notes(corduroy_extension *extension);

int32_t corduroy_extension_notes(corduroy_extension *extension) {
    if (extension->abi != CORDUROY_ABI || extension->size < sizeof *extension) {
        return CORDUROY_FAILED;
    }
    extension->name = CORDUROY_STR("notes");
    extension->inject = INJECT;
    extension->inject_len = sizeof INJECT / sizeof INJECT[0];
    /* One extension's state is its own: the host may ask for several. */
    extension->user = calloc(1, sizeof(notes));
    extension->apply = apply;
    extension->drop = free;
    return extension->user != NULL ? CORDUROY_OK : CORDUROY_FAILED;
}

/* An extension whose `apply` asks for a registry it did not list, and does not look at
 * what the host answered. */
static int32_t careless(void *user, const corduroy_host *host, corduroy_ctx *ctx) {
    (void)user;
    host->gate(ctx, CORDUROY_STR("careless-gate"), check, NULL);
    return CORDUROY_OK;
}

int32_t corduroy_extension_careless(corduroy_extension *extension);

int32_t corduroy_extension_careless(corduroy_extension *extension) {
    extension->name = CORDUROY_STR("careless");
    extension->apply = careless;
    return CORDUROY_OK;
}
