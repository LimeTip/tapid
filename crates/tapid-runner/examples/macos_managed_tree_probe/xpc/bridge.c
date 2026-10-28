// Research-only SDK bridge for Clang blocks. Control assertions live in Rust.
#include <xpc/xpc.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

typedef int64_t (*probe_control)(const char *);
static probe_control worker_control;
static void accept_peer(xpc_connection_t peer) {
    xpc_connection_set_event_handler(peer, ^(xpc_object_t event) {
        if (xpc_get_type(event) != XPC_TYPE_DICTIONARY) return;
        xpc_object_t reply = xpc_dictionary_create_reply(event);
        if (!reply) return;
        const char *operation = xpc_dictionary_get_string(event, "operation");
        const char *path = xpc_dictionary_get_string(event, "outside");
        int64_t result = -1;
        if (operation && strcmp(operation, "hello") == 0) result = 1;
        else if (operation && strcmp(operation, "control") == 0 && path)
            result = worker_control(path);
        xpc_dictionary_set_int64(reply, "control", result);
        xpc_connection_send_message(peer, reply);
        xpc_release(reply);
    });
    xpc_connection_resume(peer);
}
void probe_service_main(probe_control control) {
    worker_control = control;
    xpc_main(accept_peer);
}

static int64_t exchange(xpc_connection_t peer, xpc_object_t request, int64_t timeout_ns) {
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block int64_t result = -1;
    dispatch_retain(done);
    xpc_connection_send_message_with_reply(peer, request, dispatch_get_global_queue(0, 0), ^(xpc_object_t reply) {
        if (xpc_get_type(reply) == XPC_TYPE_DICTIONARY) {
            xpc_object_t value = xpc_dictionary_get_value(reply, "control");
            if (value && xpc_get_type(value) == XPC_TYPE_INT64)
                result = xpc_int64_get_value(value);
        }
        if (xpc_get_type(reply) == XPC_TYPE_ERROR) {
            char *description = xpc_copy_description(reply);
            fprintf(stderr, "XPC reply error: %s\n", description ? description : "unknown");
            free(description);
        }
        dispatch_semaphore_signal(done);
        dispatch_release(done);
    });
    long timeout = dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, timeout_ns));
    // The queued reply owns the captured state and a semaphore reference even
    // after timeout. No callback refers to the caller's stack or result buffer.
    dispatch_release(done);
    return timeout ? -2 : result;
}

static int64_t round_trip(xpc_connection_t peer, const char *outside, int64_t timeout_ns) {
    xpc_object_t hello = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_string(hello, "operation", "hello");
    int64_t result = exchange(peer, hello, timeout_ns);
    xpc_release(hello);
    if (result != 1) return result; // No authority payload before acknowledgement.
    xpc_object_t request = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_string(request, "operation", "control");
    xpc_dictionary_set_string(request, "outside", outside);
    result = exchange(peer, request, timeout_ns);
    xpc_release(request);
    return result;
}

int64_t probe_client(const char *outside) {
    xpc_connection_t peer = xpc_connection_create("org.tapid.research.worker", NULL);
    xpc_connection_set_event_handler(peer, ^(xpc_object_t event) {
        if (xpc_get_type(event) == XPC_TYPE_ERROR) {
            char *description = xpc_copy_description(event);
            fprintf(stderr, "XPC connection error: %s\n", description ? description : "unknown");
            free(description);
        }
    });
    xpc_connection_resume(peer);
    int64_t result = round_trip(peer, outside, 3 * NSEC_PER_SEC);
    xpc_connection_cancel(peer);
    xpc_release(peer);
    return result;
}

// Anonymous endpoints test transport without registering a service or implying
// any sandbox/identity guarantee. Modes: valid, malformed, silent, late reply.
void probe_transport_controls(int64_t *results) {
    for (int mode = 0; mode < 4; mode++) {
        xpc_connection_t listener = xpc_connection_create(NULL, dispatch_get_global_queue(0, 0));
        xpc_connection_set_event_handler(listener, ^(xpc_object_t incoming) {
            if (xpc_get_type(incoming) != XPC_TYPE_CONNECTION) return;
            xpc_connection_t server = (xpc_connection_t)incoming;
            xpc_connection_set_event_handler(server, ^(xpc_object_t event) {
                if (xpc_get_type(event) != XPC_TYPE_DICTIONARY) return;
                if (mode == 2) {
                    // Retain the request: dropping an unanswered event itself
                    // triggers an interruption reply, rather than silence.
                    xpc_retain(event);
                    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_global_queue(0, 0), ^{ xpc_release(event); });
                    return;
                }
                xpc_object_t reply = xpc_dictionary_create_reply(event);
                if (!reply) return;
                const char *operation = xpc_dictionary_get_string(event, "operation");
                if (mode == 1) xpc_dictionary_set_string(reply, "control", "malformed");
                else xpc_dictionary_set_int64(reply, "control", operation && strcmp(operation, "hello") == 0 ? 1 : 15);
                if (mode == 3) {
                    xpc_retain(server);
                    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_global_queue(0, 0), ^{
                        xpc_connection_send_message(server, reply);
                        xpc_release(reply);
                        xpc_release(server);
                    });
                } else {
                    xpc_connection_send_message(server, reply);
                    xpc_release(reply);
                }
            });
            xpc_connection_resume(server);
        });
        xpc_connection_resume(listener);
        xpc_endpoint_t endpoint = xpc_endpoint_create(listener);
        xpc_connection_t peer = xpc_connection_create_from_endpoint(endpoint);
        xpc_connection_set_event_handler(peer, ^(xpc_object_t event) { (void)event; });
        xpc_connection_resume(peer);
        results[mode] = round_trip(peer, "synthetic-only", mode < 2 ? NSEC_PER_SEC : 30 * NSEC_PER_MSEC);
        xpc_connection_cancel(peer);
        xpc_connection_cancel(listener);
        xpc_release(peer);
        xpc_release(endpoint);
        xpc_release(listener);
    }
    // Let the deliberately late block run after exchange has returned.
    dispatch_semaphore_t settle = dispatch_semaphore_create(0);
    dispatch_retain(settle);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 200 * NSEC_PER_MSEC), dispatch_get_global_queue(0, 0), ^{
        dispatch_semaphore_signal(settle);
        dispatch_release(settle);
    });
    dispatch_semaphore_wait(settle, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC));
    dispatch_release(settle);
}
