/*
 * lumen_surface_protocol.h - the producer role of the Lumen surface protocol.
 *
 * An engine shows its frames in a Lumen <surface> element, with no copy, by
 * rendering into GPU images it exports and announcing each finished frame.
 * This header is the C ABI over the protocol library (link
 * liblumen_surface_protocol, static or shared). SPEC.md beside it is the
 * protocol itself, for an engine that speaks it without the library.
 *
 * The shape of a producer:
 *
 *   LspProducer *p = lsp_connect(getenv("LUMEN_SURFACE"), &hello, &welcome, &st);
 *   // allocate welcome.ring_pref images of welcome.width x welcome.height,
 *   // export each, then:
 *   lsp_offer_buffers(p, &ring);
 *   for (;;) {
 *       LspEvent ev;
 *       while (lsp_poll(p, &ev) == LSP_OK && ev.kind != LSP_EVENT_NONE) { ... }
 *       LspSlot slot;
 *       if (lsp_next_slot(p, 16, &slot) != LSP_OK) continue;
 *       // render into ring image slot.slot, wait for the GPU, then:
 *       lsp_frame_ready(p, &frame, NULL);
 *   }
 *   lsp_close(p);
 *
 * Every function returns an LspStatus (lsp_connect writes one out); a failure
 * leaves a message lsp_last_error reads. No call lets a panic or exception
 * cross into the caller. Structs the engine fills begin with struct_size; set
 * it to sizeof the struct. A producer is used from one thread at a time.
 *
 * Licensed under the Mozilla Public License 2.0.
 */
#ifndef LUMEN_SURFACE_PROTOCOL_H
#define LUMEN_SURFACE_PROTOCOL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The protocol version this header describes: major 1, minor 0. */
#define LSP_VERSION_MAJOR 1u
#define LSP_VERSION_MINOR 0u

typedef uint32_t LspStatus;
#define LSP_OK 0u
#define LSP_ERR_ARG 1u        /* a null, too small or out-of-range argument */
#define LSP_ERR_ENDPOINT 2u   /* the endpoint string does not parse */
#define LSP_ERR_IO 3u         /* the socket failed */
#define LSP_ERR_REJECTED 4u   /* the consumer refused the producer or a ring */
#define LSP_ERR_TIMEOUT 5u    /* nothing came in time */
#define LSP_ERR_CLOSED 6u     /* the session is over */
#define LSP_ERR_MISUSE 7u     /* a call broke the protocol's rules */
#define LSP_ERR_PANIC 8u      /* an internal failure; do not use the producer again */

#define LSP_API_VULKAN 1u
#define LSP_FORMAT_RGBA8_SRGB 1u /* VK_FORMAT_R8G8B8A8_SRGB */
#define LSP_BUFFER_DEDICATED 1u  /* each buffer is a dedicated allocation */

typedef struct LspProducer LspProducer;

typedef struct LspHello {
    uint32_t struct_size;   /* sizeof(LspHello) */
    uint16_t api;           /* LSP_API_VULKAN */
    uint16_t reserved0;
    uint32_t caps;          /* 0 in version 1 */
    uint32_t reserved1;
    uint8_t device_uuid[16]; /* VkPhysicalDeviceIDProperties::deviceUUID */
    uint8_t driver_uuid[16]; /* VkPhysicalDeviceIDProperties::driverUUID */
    uint8_t luid[8];        /* 0 on Vulkan */
    uint64_t registry_id;   /* 0 on Vulkan */
    uint32_t vendor;        /* PCI vendor id */
    uint32_t device;        /* PCI device id */
    const char *engine;     /* "name version", may be NULL */
} LspHello;

typedef struct LspWelcome {
    uint32_t struct_size;   /* written by the library */
    uint16_t pacing;        /* 0 paced: one frame per request; 1 free */
    uint8_t ring_min;       /* fewest buffers in a ring */
    uint8_t ring_max;       /* most buffers in a ring */
    uint8_t ring_pref;      /* preferred buffers in a ring */
    uint8_t reserved0[3];
    uint32_t width;         /* the frame size the consumer wants */
    uint32_t height;
    float scale;            /* device pixels per logical pixel */
    uint32_t formats;       /* bit (1 << LSP_FORMAT_*) per accepted format */
    uint32_t reserved1;
    uint64_t session;
} LspWelcome;

typedef struct LspBuffer {
    uint64_t alloc_size;    /* VkMemoryRequirements::size of the image */
    uint32_t memory_type;   /* the memory type index it was allocated from */
    uint32_t offset;        /* 0 for a dedicated allocation */
    uint32_t row_pitch;     /* 0 for optimal tiling */
    uint32_t reserved0;
    uint64_t modifier;      /* 0 for OPAQUE_FD */
    int32_t fd;             /* vkGetMemoryFdKHR(OPAQUE_FD); the caller keeps it */
    uint32_t reserved1;
} LspBuffer;

typedef struct LspBuffers {
    uint32_t struct_size;   /* sizeof(LspBuffers) */
    uint32_t generation;    /* 1 for the first ring, one more for each new one */
    uint32_t width;
    uint32_t height;
    uint32_t format;        /* LSP_FORMAT_RGBA8_SRGB */
    uint32_t usage;         /* the VkImageUsageFlags the images were made with */
    uint32_t flags;         /* LSP_BUFFER_DEDICATED */
    uint32_t count;         /* 2 to 4 */
    const LspBuffer *buffers;
} LspBuffers;

typedef struct LspSlot {
    uint32_t generation;
    uint32_t slot;
} LspSlot;

typedef struct LspFrame {
    uint32_t struct_size;   /* sizeof(LspFrame) */
    uint32_t generation;    /* the slot lsp_next_slot handed out */
    uint32_t slot;
    uint32_t meta_len;      /* at most 4096 */
    const uint8_t *meta;    /* bytes the consumer's app reads with the frame */
    uint64_t t_sim;         /* CLOCK_MONOTONIC nanoseconds, 0 if unknown */
    uint64_t t_submit;
    uint64_t t_gpu_done;    /* 0 takes the moment of the call */
    uint64_t input_seq;     /* newest input applied; 0 takes the newest read */
} LspFrame;

#define LSP_EVENT_NONE 0u
#define LSP_EVENT_RESIZE 1u           /* width, height, scale: offer a new ring */
#define LSP_EVENT_INPUT 2u            /* input_kind says which */
#define LSP_EVENT_VISIBILITY 3u       /* flag: whether anyone sees the frames */
#define LSP_EVENT_USER 4u             /* data, data_len */
#define LSP_EVENT_RESET 5u            /* every buffer is gone: offer a new ring */
#define LSP_EVENT_RETIRED 6u          /* generation: free that ring */
#define LSP_EVENT_BUFFERS_REJECTED 7u /* generation, text */
#define LSP_EVENT_CLOSED 8u           /* reason, text */

#define LSP_INPUT_POINTER_MOVE 1u
#define LSP_INPUT_POINTER_DOWN 2u
#define LSP_INPUT_POINTER_UP 3u
#define LSP_INPUT_WHEEL 4u
#define LSP_INPUT_KEY 5u
#define LSP_INPUT_TEXT 6u
#define LSP_INPUT_FOCUS 7u /* flag 0: release every held key and button */
#define LSP_INPUT_LOCK 8u
#define LSP_INPUT_LEAVE 9u

#define LSP_MOD_SHIFT 1u
#define LSP_MOD_CTRL 2u
#define LSP_MOD_ALT 4u
#define LSP_MOD_META 8u

#define LSP_CURSOR_DEFAULT 0u
#define LSP_CURSOR_NONE 1u
#define LSP_CURSOR_POINTER 2u
#define LSP_CURSOR_CROSSHAIR 3u
#define LSP_CURSOR_MOVE 4u
#define LSP_CURSOR_GRAB 5u
#define LSP_CURSOR_GRABBING 6u
#define LSP_CURSOR_TEXT 7u
#define LSP_CURSOR_WAIT 8u
#define LSP_CURSOR_NOT_ALLOWED 9u

/* One event, flattened: the fields its kind uses are set, the rest 0.
 * text and data stay valid until the next call on the producer. */
typedef struct LspEvent {
    uint32_t struct_size;
    uint32_t kind;          /* LSP_EVENT_* */
    uint64_t seq;           /* input sequence number */
    uint64_t time;          /* input time, CLOCK_MONOTONIC nanoseconds */
    uint32_t input_kind;    /* LSP_INPUT_* */
    uint32_t width;         /* resize */
    uint32_t height;
    float scale;
    uint32_t generation;    /* retired or rejected ring */
    uint32_t flag;          /* visibility, focus, lock */
    float x, y;             /* pointer or wheel position, texture pixels */
    float dx, dy;           /* pointer motion (raw under a lock) or wheel scroll */
    uint32_t buttons;       /* DOM buttons mask */
    uint32_t pointer_id;    /* the mouse is 1 */
    uint16_t pointer_type;  /* 0 mouse, 1 pen, 2 touch */
    int16_t button;         /* DOM button that changed, -1 none */
    float pressure, tilt_x, tilt_y, twist;
    uint16_t wheel_unit;    /* 0 pixels, 1 lines */
    uint16_t key_code;      /* key-code table number: lsp_key_code_name */
    uint8_t pressed;
    uint8_t repeat;
    uint16_t modifiers;     /* LSP_MOD_* */
    uint32_t reason;        /* closed: 0 unknown, 1 exit, 2 device lost, 3 protocol, 4 closed */
    uint32_t reserved0;
    uint32_t reserved1;
    const char *text;       /* key name, committed text, or a reason */
    const uint8_t *data;    /* user bytes */
    uint64_t data_len;
} LspEvent;

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
_Static_assert(sizeof(void *) != 8 || sizeof(LspHello) == 80, "LspHello layout");
_Static_assert(sizeof(void *) != 8 || sizeof(LspWelcome) == 40, "LspWelcome layout");
_Static_assert(sizeof(LspBuffer) == 40, "LspBuffer layout");
_Static_assert(sizeof(void *) != 8 || sizeof(LspBuffers) == 40, "LspBuffers layout");
_Static_assert(sizeof(void *) != 8 || sizeof(LspFrame) == 56, "LspFrame layout");
_Static_assert(sizeof(void *) != 8 || sizeof(LspEvent) == 136, "LspEvent layout");
#endif

/* The protocol version the library speaks: major << 16 | minor. */
uint32_t lsp_version(void);

/* Copy the calling thread's last error message into buf (NUL-terminated,
 * truncated to len). Returns the message's full length without the NUL. */
size_t lsp_last_error(char *buf, size_t len);

/* Connect to the consumer the endpoint string names, introduce the producer,
 * and wait for the answer (at most 5 s). Returns NULL with *status set on
 * failure; LSP_ERR_REJECTED's message names the reason and the consumer's GPU. */
LspProducer *lsp_connect(const char *endpoint, const LspHello *hello, LspWelcome *welcome,
                         LspStatus *status);

/* Offer a ring and wait for the consumer to import it. The descriptors are
 * duplicated; the caller keeps its own. */
LspStatus lsp_offer_buffers(LspProducer *p, const LspBuffers *buffers);

/* Wait at most timeout_ms for a slot to render into: free, and in paced mode
 * asked for. LSP_ERR_TIMEOUT when none came. */
LspStatus lsp_next_slot(LspProducer *p, uint32_t timeout_ms, LspSlot *out);

/* Announce a finished frame; its GPU work must have completed. */
LspStatus lsp_frame_ready(LspProducer *p, const LspFrame *frame, uint64_t *frame_number);

/* Give back a slot without a frame in it. */
LspStatus lsp_abandon_slot(LspProducer *p, LspSlot slot);

/* Take the next event without waiting; out->kind is LSP_EVENT_NONE when
 * nothing waits. Call it at least once a second: it answers the consumer's
 * pings. */
LspStatus lsp_poll(LspProducer *p, LspEvent *out);

/* The cursor over the surface: LSP_CURSOR_*. */
LspStatus lsp_set_cursor(LspProducer *p, uint16_t keyword);

/* Ask for the pointer lock (wanted 1) or let it go (0). The consumer decides;
 * an LSP_INPUT_LOCK input says when it took effect. */
LspStatus lsp_request_pointer_lock(LspProducer *p, uint8_t wanted);

/* Send bytes to the consumer's application, at most 61440. */
LspStatus lsp_send_user(LspProducer *p, const uint8_t *bytes, uint32_t len);

/* End the session and free the producer. NULL is ignored. */
void lsp_close(LspProducer *p);

/* The consumer's GPU from an endpoint string: the Vulkan deviceUUID and
 * driverUUID (16 bytes each) to pick a physical device by before making the
 * producer's device. LSP_ERR_ARG when the string names none. */
LspStatus lsp_endpoint_adapter(const char *endpoint, uint8_t *device_uuid, uint8_t *driver_uuid);

/* The DOM code name ("KeyW", "ShiftLeft") of a key code; a static string. */
const char *lsp_key_code_name(uint16_t code);

#ifdef __cplusplus
}
#endif

#endif /* LUMEN_SURFACE_PROTOCOL_H */
