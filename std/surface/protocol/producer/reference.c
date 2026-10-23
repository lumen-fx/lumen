/*
 * The reference producer: a C program that shows a test pattern in a Lumen
 * <surface> through the surface protocol, with Vulkan and no window.
 *
 * It does what every producer does, in the plainest form: pick the GPU the
 * endpoint names, make a device with external memory, connect, allocate and
 * export a ring of images, then render into each slot the consumer frees
 * and announce it once the GPU has finished. Input moves the pattern, so a
 * test sees input reach the producer in the frames that come back.
 *
 * The pattern: the whole frame cleared to the background colour, and a
 * square of SQUARE texture pixels at the last pointer position (the middle
 * until a pointer event arrives). Each key press swaps the background
 * between blue and green.
 *
 * Environment:
 *   LUMEN_SURFACE         the endpoint string (required)
 *   LSP_REF_FRAMES        exit cleanly after this many frames (0: until the
 *                         consumer ends the session)
 *   LSP_REF_CRASH_AFTER   exit without a goodbye after this many frames, as a
 *                         crash would
 *
 * Build: cc reference.c -I../include -L<dir of liblumen_surface_protocol>
 *        -llumen_surface_protocol -lvulkan
 *
 * Licensed under the Mozilla Public License 2.0.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include <vulkan/vulkan.h>

#include "lumen_surface_protocol.h"

#define MAX_SLOTS 4
#define SQUARE 16

#define CHECK(call, what)                                                           \
    do {                                                                            \
        VkResult r_ = (call);                                                       \
        if (r_ != VK_SUCCESS) {                                                     \
            fprintf(stderr, "lsp-reference: %s failed (%d)\n", what, (int)r_);      \
            exit(2);                                                                \
        }                                                                           \
    } while (0)

static void log_line(const char *fmt, const char *arg) {
    fprintf(stderr, "lsp-reference: ");
    fprintf(stderr, fmt, arg);
    fprintf(stderr, "\n");
}

typedef struct Gpu {
    VkInstance instance;
    VkPhysicalDevice physical;
    VkDevice device;
    VkQueue queue;
    uint32_t family;
    VkCommandPool pool;
    VkRenderPass pass;
    PFN_vkGetMemoryFdKHR get_memory_fd;
    uint8_t device_uuid[16];
    uint8_t driver_uuid[16];
    uint32_t vendor;
    uint32_t device_id;
} Gpu;

typedef struct Slot {
    VkImage image;
    VkDeviceMemory memory;
    VkImageView view;
    VkFramebuffer framebuffer;
    VkCommandBuffer commands;
    VkFence fence;
    int used; /* rendered into at least once: comes back from the consumer */
} Slot;

typedef struct Ring {
    uint32_t generation;
    uint32_t width, height;
    uint32_t count;
    Slot slots[MAX_SLOTS];
    int live;
} Ring;

static const VkImageUsageFlags USAGE = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT |
                                      VK_IMAGE_USAGE_SAMPLED_BIT |
                                      VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
                                      VK_IMAGE_USAGE_TRANSFER_DST_BIT;

/* Make an instance and a device on the GPU the endpoint names (the first one
 * when it names none), with exportable memory. */
static void open_gpu(Gpu *g, const char *endpoint) {
    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO};
    app.pApplicationName = "lsp-reference";
    app.apiVersion = VK_API_VERSION_1_1;
    VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
    ici.pApplicationInfo = &app;
    CHECK(vkCreateInstance(&ici, NULL, &g->instance), "vkCreateInstance");

    uint8_t want_device[16], want_driver[16];
    int want = lsp_endpoint_adapter(endpoint, want_device, want_driver) == LSP_OK;

    uint32_t count = 0;
    CHECK(vkEnumeratePhysicalDevices(g->instance, &count, NULL), "vkEnumeratePhysicalDevices");
    if (count == 0) {
        fprintf(stderr, "lsp-reference: no Vulkan device\n");
        exit(2);
    }
    VkPhysicalDevice devices[16];
    if (count > 16) count = 16;
    CHECK(vkEnumeratePhysicalDevices(g->instance, &count, devices), "vkEnumeratePhysicalDevices");
    g->physical = VK_NULL_HANDLE;
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceIDProperties id = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
        VkPhysicalDeviceProperties2 p2 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2};
        p2.pNext = &id;
        vkGetPhysicalDeviceProperties2(devices[i], &p2);
        int match = !want || (memcmp(id.deviceUUID, want_device, 16) == 0 &&
                              memcmp(id.driverUUID, want_driver, 16) == 0);
        if (match) {
            g->physical = devices[i];
            memcpy(g->device_uuid, id.deviceUUID, 16);
            memcpy(g->driver_uuid, id.driverUUID, 16);
            g->vendor = p2.properties.vendorID;
            g->device_id = p2.properties.deviceID;
            log_line("device %s", p2.properties.deviceName);
            break;
        }
    }
    if (g->physical == VK_NULL_HANDLE) {
        fprintf(stderr, "lsp-reference: the consumer's GPU is not among this machine's devices\n");
        exit(2);
    }

    uint32_t families = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(g->physical, &families, NULL);
    VkQueueFamilyProperties props[16];
    if (families > 16) families = 16;
    vkGetPhysicalDeviceQueueFamilyProperties(g->physical, &families, props);
    g->family = UINT32_MAX;
    for (uint32_t i = 0; i < families; i++) {
        if (props[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            g->family = i;
            break;
        }
    }
    if (g->family == UINT32_MAX) {
        fprintf(stderr, "lsp-reference: no graphics queue\n");
        exit(2);
    }
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    qci.queueFamilyIndex = g->family;
    qci.queueCount = 1;
    qci.pQueuePriorities = &priority;
    const char *extensions[] = {VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME};
    VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
    dci.queueCreateInfoCount = 1;
    dci.pQueueCreateInfos = &qci;
    dci.enabledExtensionCount = 1;
    dci.ppEnabledExtensionNames = extensions;
    CHECK(vkCreateDevice(g->physical, &dci, NULL, &g->device), "vkCreateDevice");
    vkGetDeviceQueue(g->device, g->family, 0, &g->queue);
    g->get_memory_fd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(g->device, "vkGetMemoryFdKHR");
    if (!g->get_memory_fd) {
        fprintf(stderr, "lsp-reference: no vkGetMemoryFdKHR\n");
        exit(2);
    }

    VkCommandPoolCreateInfo pci = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
    pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pci.queueFamilyIndex = g->family;
    CHECK(vkCreateCommandPool(g->device, &pci, NULL, &g->pool), "vkCreateCommandPool");

    /* One pass: clear to the background, keep the result. The image arrives
     * in COLOR_ATTACHMENT_OPTIMAL (a barrier before the pass puts it there). */
    VkAttachmentDescription color = {0};
    color.format = VK_FORMAT_R8G8B8A8_SRGB;
    color.samples = VK_SAMPLE_COUNT_1_BIT;
    color.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR;
    color.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    color.stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
    color.stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    color.initialLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    color.finalLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    VkAttachmentReference ref = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {0};
    sub.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS;
    sub.colorAttachmentCount = 1;
    sub.pColorAttachments = &ref;
    VkRenderPassCreateInfo rpci = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO};
    rpci.attachmentCount = 1;
    rpci.pAttachments = &color;
    rpci.subpassCount = 1;
    rpci.pSubpasses = &sub;
    CHECK(vkCreateRenderPass(g->device, &rpci, NULL, &g->pass), "vkCreateRenderPass");
}

static uint32_t device_local_type(Gpu *g, uint32_t bits) {
    VkPhysicalDeviceMemoryProperties mem;
    vkGetPhysicalDeviceMemoryProperties(g->physical, &mem);
    for (uint32_t i = 0; i < mem.memoryTypeCount; i++) {
        if ((bits & (1u << i)) &&
            (mem.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) {
            return i;
        }
    }
    for (uint32_t i = 0; i < mem.memoryTypeCount; i++) {
        if (bits & (1u << i)) return i;
    }
    fprintf(stderr, "lsp-reference: no memory type for the images\n");
    exit(2);
}

/* Allocate a ring, export it, and offer it to the consumer. */
static int make_ring(Gpu *g, LspProducer *p, Ring *ring, uint32_t generation, uint32_t width,
                     uint32_t height, uint32_t count) {
    memset(ring, 0, sizeof *ring);
    ring->generation = generation;
    ring->width = width;
    ring->height = height;
    ring->count = count;
    LspBuffer buffers[MAX_SLOTS];
    memset(buffers, 0, sizeof buffers);
    for (uint32_t i = 0; i < count; i++) {
        Slot *s = &ring->slots[i];
        VkExternalMemoryImageCreateInfo ext = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO};
        ext.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
        VkImageCreateInfo ici = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
        ici.pNext = &ext;
        ici.imageType = VK_IMAGE_TYPE_2D;
        ici.format = VK_FORMAT_R8G8B8A8_SRGB;
        ici.extent.width = width;
        ici.extent.height = height;
        ici.extent.depth = 1;
        ici.mipLevels = 1;
        ici.arrayLayers = 1;
        ici.samples = VK_SAMPLE_COUNT_1_BIT;
        ici.tiling = VK_IMAGE_TILING_OPTIMAL;
        ici.usage = USAGE;
        ici.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
        ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
        CHECK(vkCreateImage(g->device, &ici, NULL, &s->image), "vkCreateImage");

        VkMemoryRequirements req;
        vkGetImageMemoryRequirements(g->device, s->image, &req);
        uint32_t type = device_local_type(g, req.memoryTypeBits);
        VkExportMemoryAllocateInfo export_info = {VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO};
        export_info.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
        VkMemoryDedicatedAllocateInfo dedicated = {VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
        dedicated.image = s->image;
        export_info.pNext = &dedicated;
        VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
        mai.pNext = &export_info;
        mai.allocationSize = req.size;
        mai.memoryTypeIndex = type;
        CHECK(vkAllocateMemory(g->device, &mai, NULL, &s->memory), "vkAllocateMemory");
        CHECK(vkBindImageMemory(g->device, s->image, s->memory, 0), "vkBindImageMemory");

        VkMemoryGetFdInfoKHR get = {VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR};
        get.memory = s->memory;
        get.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
        int fd = -1;
        CHECK(g->get_memory_fd(g->device, &get, &fd), "vkGetMemoryFdKHR");
        struct stat st;
        if (fstat(fd, &st) == 0) {
            fprintf(stderr, "lsp-reference: buffer %u.%u inode %llu\n", generation, i,
                    (unsigned long long)st.st_ino);
        }
        buffers[i].alloc_size = req.size;
        buffers[i].memory_type = type;
        buffers[i].fd = fd;

        VkImageViewCreateInfo vci = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO};
        vci.image = s->image;
        vci.viewType = VK_IMAGE_VIEW_TYPE_2D;
        vci.format = VK_FORMAT_R8G8B8A8_SRGB;
        vci.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
        vci.subresourceRange.levelCount = 1;
        vci.subresourceRange.layerCount = 1;
        CHECK(vkCreateImageView(g->device, &vci, NULL, &s->view), "vkCreateImageView");
        VkFramebufferCreateInfo fci = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO};
        fci.renderPass = g->pass;
        fci.attachmentCount = 1;
        fci.pAttachments = &s->view;
        fci.width = width;
        fci.height = height;
        fci.layers = 1;
        CHECK(vkCreateFramebuffer(g->device, &fci, NULL, &s->framebuffer), "vkCreateFramebuffer");
        VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
        cai.commandPool = g->pool;
        cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
        cai.commandBufferCount = 1;
        CHECK(vkAllocateCommandBuffers(g->device, &cai, &s->commands), "vkAllocateCommandBuffers");
        VkFenceCreateInfo fen = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
        CHECK(vkCreateFence(g->device, &fen, NULL, &s->fence), "vkCreateFence");
    }
    LspBuffers ring_desc = {0};
    ring_desc.struct_size = sizeof ring_desc;
    ring_desc.generation = generation;
    ring_desc.width = width;
    ring_desc.height = height;
    ring_desc.format = LSP_FORMAT_RGBA8_SRGB;
    ring_desc.usage = (uint32_t)USAGE;
    ring_desc.flags = LSP_BUFFER_DEDICATED;
    ring_desc.count = count;
    ring_desc.buffers = buffers;
    LspStatus st = lsp_offer_buffers(p, &ring_desc);
    /* The consumer holds its own duplicates now. */
    for (uint32_t i = 0; i < count; i++) close(buffers[i].fd);
    if (st != LSP_OK) {
        char msg[256];
        lsp_last_error(msg, sizeof msg);
        fprintf(stderr, "lsp-reference: the consumer refused the ring: %s\n", msg);
        return 0;
    }
    ring->live = 1;
    fprintf(stderr, "lsp-reference: ring %u is %ux%u, %u buffers\n", generation, width, height,
            count);
    return 1;
}

static void free_ring(Gpu *g, Ring *ring) {
    if (!ring->live) return;
    vkDeviceWaitIdle(g->device);
    for (uint32_t i = 0; i < ring->count; i++) {
        Slot *s = &ring->slots[i];
        vkDestroyFence(g->device, s->fence, NULL);
        vkFreeCommandBuffers(g->device, g->pool, 1, &s->commands);
        vkDestroyFramebuffer(g->device, s->framebuffer, NULL);
        vkDestroyImageView(g->device, s->view, NULL);
        vkDestroyImage(g->device, s->image, NULL);
        vkFreeMemory(g->device, s->memory, NULL);
    }
    ring->live = 0;
}

static void barrier(VkCommandBuffer cb, VkImage image, VkImageLayout from, VkImageLayout to,
                    uint32_t src_family, uint32_t dst_family, VkAccessFlags src_access,
                    VkAccessFlags dst_access, VkPipelineStageFlags src_stage,
                    VkPipelineStageFlags dst_stage) {
    VkImageMemoryBarrier b = {VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
    b.srcAccessMask = src_access;
    b.dstAccessMask = dst_access;
    b.oldLayout = from;
    b.newLayout = to;
    b.srcQueueFamilyIndex = src_family;
    b.dstQueueFamilyIndex = dst_family;
    b.image = image;
    b.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
    b.subresourceRange.levelCount = 1;
    b.subresourceRange.layerCount = 1;
    vkCmdPipelineBarrier(cb, src_stage, dst_stage, 0, 0, NULL, 0, NULL, 1, &b);
}

/* Render the pattern into a slot and wait for the GPU to finish it. */
static void render(Gpu *g, Ring *ring, uint32_t index, float bg[4], float px, float py) {
    Slot *s = &ring->slots[index];
    VkCommandBuffer cb = s->commands;
    vkResetCommandBuffer(cb, 0);
    VkCommandBufferBeginInfo begin = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    CHECK(vkBeginCommandBuffer(cb, &begin), "vkBeginCommandBuffer");
    /* Take the image back from the consumer, which released it to the
     * external family in the shared layout. The acquire names the same
     * layouts as that release, so it changes none; the move to the layout
     * the pass draws in is a barrier of its own. A fresh image has no
     * contents to keep and no owner to take it from. */
    if (s->used) {
        barrier(cb, s->image, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_QUEUE_FAMILY_EXTERNAL, g->family, 0, 0,
                VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT);
        barrier(cb, s->image, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_QUEUE_FAMILY_IGNORED,
                VK_QUEUE_FAMILY_IGNORED, 0, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
                VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT);
    } else {
        barrier(cb, s->image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, 0,
                VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT);
    }
    VkClearValue clear;
    memcpy(clear.color.float32, bg, sizeof(float) * 4);
    VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO};
    rbi.renderPass = g->pass;
    rbi.framebuffer = s->framebuffer;
    rbi.renderArea.extent.width = ring->width;
    rbi.renderArea.extent.height = ring->height;
    rbi.clearValueCount = 1;
    rbi.pClearValues = &clear;
    vkCmdBeginRenderPass(cb, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    int32_t x = (int32_t)px - SQUARE / 2, y = (int32_t)py - SQUARE / 2;
    if (x < 0) x = 0;
    if (y < 0) y = 0;
    if ((uint32_t)x + SQUARE <= ring->width && (uint32_t)y + SQUARE <= ring->height) {
        VkClearAttachment att = {0};
        att.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
        att.colorAttachment = 0;
        att.clearValue.color.float32[0] = 1.0f;
        att.clearValue.color.float32[1] = 0.5f;
        att.clearValue.color.float32[2] = 0.0f;
        att.clearValue.color.float32[3] = 1.0f;
        VkClearRect rect = {0};
        rect.rect.offset.x = x;
        rect.rect.offset.y = y;
        rect.rect.extent.width = SQUARE;
        rect.rect.extent.height = SQUARE;
        rect.layerCount = 1;
        vkCmdClearAttachments(cb, 1, &att, 1, &rect);
    }
    vkCmdEndRenderPass(cb);
    /* Hand it over: into the shared layout on this family first, then
     * released to the external family with no change of layout, which is
     * what the consumer's acquire names. */
    barrier(cb, s->image, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_QUEUE_FAMILY_IGNORED,
            VK_QUEUE_FAMILY_IGNORED, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, 0,
            VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
    barrier(cb, s->image, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
            VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, g->family, VK_QUEUE_FAMILY_EXTERNAL, 0, 0,
            VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT);
    CHECK(vkEndCommandBuffer(cb), "vkEndCommandBuffer");
    VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO};
    si.commandBufferCount = 1;
    si.pCommandBuffers = &cb;
    CHECK(vkResetFences(g->device, 1, &s->fence), "vkResetFences");
    CHECK(vkQueueSubmit(g->queue, 1, &si, s->fence), "vkQueueSubmit");
    CHECK(vkWaitForFences(g->device, 1, &s->fence, VK_TRUE, UINT64_MAX), "vkWaitForFences");
    s->used = 1;
}

static uint32_t env_u32(const char *name) {
    const char *v = getenv(name);
    return v ? (uint32_t)strtoul(v, NULL, 10) : 0;
}

int main(void) {
    const char *endpoint = getenv("LUMEN_SURFACE");
    if (!endpoint) {
        fprintf(stderr, "lsp-reference: LUMEN_SURFACE is not set\n");
        return 2;
    }
    uint32_t stop_after = env_u32("LSP_REF_FRAMES");
    uint32_t crash_after = env_u32("LSP_REF_CRASH_AFTER");

    Gpu g;
    memset(&g, 0, sizeof g);
    open_gpu(&g, endpoint);

    LspHello hello;
    memset(&hello, 0, sizeof hello);
    hello.struct_size = sizeof hello;
    hello.api = LSP_API_VULKAN;
    memcpy(hello.device_uuid, g.device_uuid, 16);
    memcpy(hello.driver_uuid, g.driver_uuid, 16);
    hello.vendor = g.vendor;
    hello.device = g.device_id;
    hello.engine = "lsp-reference 1";
    LspWelcome welcome;
    LspStatus st = 0;
    LspProducer *p = lsp_connect(endpoint, &hello, &welcome, &st);
    if (!p) {
        char msg[512];
        lsp_last_error(msg, sizeof msg);
        fprintf(stderr, "lsp-reference: connect failed (%u): %s\n", st, msg);
        return 3;
    }
    fprintf(stderr, "lsp-reference: welcomed at %ux%u, %s pacing\n", welcome.width,
            welcome.height, welcome.pacing ? "free" : "paced");

    uint32_t ring_count = welcome.ring_pref;
    if (ring_count < welcome.ring_min) ring_count = welcome.ring_min;
    if (ring_count > welcome.ring_max) ring_count = welcome.ring_max;
    if (ring_count > MAX_SLOTS) ring_count = MAX_SLOTS;

    /* The ring in use, and the one before it until the consumer retires it. */
    Ring rings[2];
    memset(rings, 0, sizeof rings);
    int current = 0;
    uint32_t generation = 1;
    if (!make_ring(&g, p, &rings[current], generation, welcome.width, welcome.height, ring_count)) {
        return 4;
    }

    float blue[4] = {0.0f, 0.0f, 0.8f, 1.0f};
    float green[4] = {0.0f, 0.8f, 0.0f, 1.0f};
    int green_bg = 0;
    float px = (float)welcome.width / 2.0f, py = (float)welcome.height / 2.0f;
    uint32_t frames = 0;
    for (;;) {
        LspEvent ev;
        while (lsp_poll(p, &ev) == LSP_OK && ev.kind != LSP_EVENT_NONE) {
            switch (ev.kind) {
                case LSP_EVENT_INPUT:
                    if (ev.input_kind == LSP_INPUT_POINTER_MOVE ||
                        ev.input_kind == LSP_INPUT_POINTER_DOWN) {
                        px = ev.x;
                        py = ev.y;
                    } else if (ev.input_kind == LSP_INPUT_KEY && ev.pressed && !ev.repeat) {
                        green_bg = !green_bg;
                        fprintf(stderr, "lsp-reference: key %s\n", lsp_key_code_name(ev.key_code));
                    }
                    break;
                case LSP_EVENT_RESIZE: {
                    int next = 1 - current;
                    free_ring(&g, &rings[next]);
                    generation++;
                    if (make_ring(&g, p, &rings[next], generation, ev.width, ev.height,
                                  ring_count)) {
                        current = next;
                        px = (float)ev.width / 2.0f;
                        py = (float)ev.height / 2.0f;
                    }
                    break;
                }
                case LSP_EVENT_RETIRED:
                    for (int i = 0; i < 2; i++) {
                        if (rings[i].live && rings[i].generation == ev.generation && i != current) {
                            free_ring(&g, &rings[i]);
                        }
                    }
                    break;
                case LSP_EVENT_RESET:
                    free_ring(&g, &rings[0]);
                    free_ring(&g, &rings[1]);
                    generation++;
                    current = 0;
                    make_ring(&g, p, &rings[current], generation, welcome.width, welcome.height,
                              ring_count);
                    break;
                case LSP_EVENT_CLOSED:
                    fprintf(stderr, "lsp-reference: the consumer ended the session\n");
                    lsp_close(p);
                    return 0;
                default:
                    break;
            }
        }
        LspSlot slot;
        st = lsp_next_slot(p, 50, &slot);
        if (st == LSP_ERR_TIMEOUT) continue;
        if (st != LSP_OK) {
            char msg[256];
            lsp_last_error(msg, sizeof msg);
            fprintf(stderr, "lsp-reference: %s\n", msg);
            lsp_close(p);
            return st == LSP_ERR_CLOSED ? 0 : 5;
        }
        Ring *ring = &rings[current];
        if (slot.generation != ring->generation || slot.slot >= ring->count) {
            lsp_abandon_slot(p, slot);
            continue;
        }
        render(&g, ring, slot.slot, green_bg ? green : blue, px, py);
        LspFrame frame;
        memset(&frame, 0, sizeof frame);
        frame.struct_size = sizeof frame;
        frame.generation = slot.generation;
        frame.slot = slot.slot;
        uint8_t meta[8];
        memcpy(meta, &frames, sizeof frames);
        memcpy(meta + 4, &generation, sizeof generation);
        frame.meta = meta;
        frame.meta_len = sizeof meta;
        if (lsp_frame_ready(p, &frame, NULL) != LSP_OK) {
            lsp_close(p);
            return 0;
        }
        frames++;
        if (crash_after && frames >= crash_after) {
            fprintf(stderr, "lsp-reference: exiting without a goodbye after %u frames\n", frames);
            _exit(7);
        }
        if (stop_after && frames >= stop_after) {
            fprintf(stderr, "lsp-reference: done after %u frames\n", frames);
            lsp_close(p);
            free_ring(&g, &rings[0]);
            free_ring(&g, &rings[1]);
            return 0;
        }
    }
}
