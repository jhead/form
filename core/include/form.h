/*
 * form — C ABI over form-core.
 *
 * Canonical header. Committed so the Swift package builds without running cbindgen.
 * TODO(W6): generate this with cbindgen and add the drift test from docs/specs/06-ffi.md §1.
 *
 * Contract (docs/specs/00-protocol.md):
 *   - Every payload is UTF-8 JSON. Nothing else crosses this boundary.
 *   - Strings returned by form_core_query / form_core_dispatch are owned by the caller and
 *     must be released with form_string_free.
 *   - Events are delivered on one dedicated thread, in order, never concurrently. The
 *     callback must not re-enter the core.
 */

#ifndef FORM_H
#define FORM_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped on any breaking protocol change. The client asserts a match at startup. */
#define FORM_ABI_VERSION 1

typedef struct FormCoreHandle FormCoreHandle;

/*
 * Receives one serialized event per call. `json` is valid only for the duration of the
 * call — copy it. `ctx` is the opaque pointer passed to form_core_subscribe.
 */
typedef void (*FormEventCallback)(const char *json, size_t len, void *ctx);

uint32_t form_abi_version(void);

/* Returns NULL on failure; form_last_error() explains why. */
FormCoreHandle *form_core_new(const char *config_json);

/* Safe to call while a run is streaming. */
void form_core_free(FormCoreHandle *handle);

/* Returns a positive token, or -1 on failure. */
int32_t form_core_subscribe(FormCoreHandle *handle, FormEventCallback callback, void *ctx);

/* After this returns, the callback is guaranteed not to be invoked again. */
void form_core_unsubscribe(FormCoreHandle *handle, int32_t token);

/* Synchronous read. Never returns NULL; failures come back as an error envelope. */
char *form_core_query(FormCoreHandle *handle, const char *query_json);

/* Asynchronous command. Returns an ack envelope; outcomes arrive as events. */
char *form_core_dispatch(FormCoreHandle *handle, const char *command_json);

void form_string_free(char *s);

/* Last error on the calling thread, valid until that thread's next failing call. */
const char *form_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* FORM_H */
