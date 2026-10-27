// Research-only SDK bridge for Clang blocks. All control assertions live in Rust.
#include <xpc/xpc.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>

typedef int64_t (*probe_control)(const char *);
static probe_control worker_control;
static void accept_peer(xpc_connection_t peer) {
        xpc_connection_set_event_handler(peer, ^(xpc_object_t event) {
            if (xpc_get_type(event) != XPC_TYPE_DICTIONARY) return;
            xpc_object_t reply = xpc_dictionary_create_reply(event);
            if (!reply) return;
            const char *path = xpc_dictionary_get_string(event, "outside");
            xpc_dictionary_set_int64(reply, "control", path ? worker_control(path) : -1);
            xpc_connection_send_message(peer, reply);
            xpc_release(reply);
        });
        xpc_connection_resume(peer);
}
void probe_service_main(probe_control control) {
    worker_control = control;
    xpc_main(accept_peer);
}

int64_t probe_client(const char *outside) {
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block int64_t result = -1;
    xpc_connection_t peer = xpc_connection_create("org.tapid.research.worker", NULL);
    xpc_connection_set_event_handler(peer, ^(xpc_object_t event) {
        if (xpc_get_type(event) == XPC_TYPE_ERROR) {
            char *description = xpc_copy_description(event);
            fprintf(stderr, "XPC connection error: %s\n", description ? description : "unknown");
            free(description);
        }
    });
    xpc_connection_resume(peer);
    xpc_object_t request = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_string(request, "outside", outside);
    dispatch_retain(done);
    xpc_connection_send_message_with_reply(peer, request, dispatch_get_global_queue(0, 0), ^(xpc_object_t reply) {
        if (xpc_get_type(reply) == XPC_TYPE_DICTIONARY)
            result = xpc_dictionary_get_int64(reply, "control");
        if (xpc_get_type(reply) == XPC_TYPE_ERROR) {
            char *description = xpc_copy_description(reply);
            fprintf(stderr, "XPC reply error: %s\n", description ? description : "unknown");
            free(description);
        }
        dispatch_semaphore_signal(done);
        dispatch_release(done);
    });
    xpc_release(request);
    long timeout = dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC));
    xpc_connection_cancel(peer);
    xpc_release(peer);
    // A queued reply retains its block and the captured semaphore until completion.
    dispatch_release(done);
    return timeout ? -2 : result;
}
