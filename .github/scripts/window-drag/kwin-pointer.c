// Pointer and Escape for KWin through its fake-input protocol, from stdin:
// `m X Y` moves to a desktop point, `p` and `r` press and release the left
// button, `k` presses Escape. KWin offers the protocol to every client when
// it runs with KWIN_WAYLAND_NO_PERMISSION_CHECKS=1, as wayland.sh starts it.
#include <linux/input-event-codes.h>
#include <stdio.h>
#include <string.h>
#include <wayland-client.h>
#include "fake-input-client.h"

static struct org_kde_kwin_fake_input *input;

static void global(void *data, struct wl_registry *r, uint32_t name, const char *iface, uint32_t version) {
    if (!strcmp(iface, org_kde_kwin_fake_input_interface.name))
        input = wl_registry_bind(r, name, &org_kde_kwin_fake_input_interface, version < 4 ? version : 4);
}

static void global_remove(void *data, struct wl_registry *r, uint32_t name) {}

static const struct wl_registry_listener listener = {global, global_remove};

int main(void) {
    struct wl_display *d = wl_display_connect(NULL);
    if (!d) { fprintf(stderr, "kwin-pointer: no Wayland display\n"); return 1; }
    struct wl_registry *r = wl_display_get_registry(d);
    wl_registry_add_listener(r, &listener, NULL);
    wl_display_roundtrip(d);
    if (!input) { fprintf(stderr, "kwin-pointer: KWin offers no fake input\n"); return 1; }
    org_kde_kwin_fake_input_authenticate(input, "lumen window-drag", "drives a drag test");
    wl_display_roundtrip(d);
    char line[128];
    while (fgets(line, sizeof line, stdin)) {
        int x, y;
        if (sscanf(line, "m %d %d", &x, &y) == 2) {
            org_kde_kwin_fake_input_pointer_motion_absolute(input, wl_fixed_from_int(x), wl_fixed_from_int(y));
        } else if (line[0] == 'p' || line[0] == 'r') {
            org_kde_kwin_fake_input_button(input, BTN_LEFT, line[0] == 'p' ? 1 : 0);
        } else if (line[0] == 'k') {
            org_kde_kwin_fake_input_keyboard_key(input, KEY_ESC, 1);
            org_kde_kwin_fake_input_keyboard_key(input, KEY_ESC, 0);
        }
        wl_display_roundtrip(d);
    }
    return 0;
}
