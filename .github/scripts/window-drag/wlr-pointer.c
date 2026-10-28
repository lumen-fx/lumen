// Pointer and Escape for a wlroots compositor, from stdin: `m X Y` moves to
// a desktop point, `p` and `r` press and release the left button, `k`
// presses Escape (through wtype, which speaks virtual-keyboard). One line a
// command; the extent is the output's size, given as arguments.
#include <linux/input-event-codes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <wayland-client.h>
#include "wlr-virtual-pointer-client.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static struct wl_seat *seat;

static void global(void *data, struct wl_registry *r, uint32_t name, const char *iface, uint32_t version) {
    if (!strcmp(iface, zwlr_virtual_pointer_manager_v1_interface.name))
        manager = wl_registry_bind(r, name, &zwlr_virtual_pointer_manager_v1_interface, 1);
    else if (!strcmp(iface, wl_seat_interface.name))
        seat = wl_registry_bind(r, name, &wl_seat_interface, 1);
}

static void global_remove(void *data, struct wl_registry *r, uint32_t name) {}

static const struct wl_registry_listener listener = {global, global_remove};

static uint32_t now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

int main(int argc, char **argv) {
    uint32_t w = argc > 2 ? atoi(argv[1]) : 1280, h = argc > 2 ? atoi(argv[2]) : 1024;
    struct wl_display *d = wl_display_connect(NULL);
    if (!d) { fprintf(stderr, "wlr-pointer: no Wayland display\n"); return 1; }
    struct wl_registry *r = wl_display_get_registry(d);
    wl_registry_add_listener(r, &listener, NULL);
    wl_display_roundtrip(d);
    if (!manager) { fprintf(stderr, "wlr-pointer: the compositor offers no virtual pointer\n"); return 1; }
    struct zwlr_virtual_pointer_v1 *p = zwlr_virtual_pointer_manager_v1_create_virtual_pointer(manager, seat);
    char line[128];
    while (fgets(line, sizeof line, stdin)) {
        int x, y;
        if (sscanf(line, "m %d %d", &x, &y) == 2) {
            zwlr_virtual_pointer_v1_motion_absolute(p, now(), x, y, w, h);
        } else if (line[0] == 'p' || line[0] == 'r') {
            zwlr_virtual_pointer_v1_button(p, now(), BTN_LEFT, line[0] == 'p');
        } else if (line[0] == 'k') {
            if (system("wtype -k Escape") != 0) fprintf(stderr, "wlr-pointer: wtype failed\n");
        }
        zwlr_virtual_pointer_v1_frame(p);
        wl_display_roundtrip(d);
    }
    return 0;
}
