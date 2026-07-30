/* Margin - a tiny markdown line classifier in C.
 *
 * The candela app (main.cdl) imports this as a shared library and calls it once
 * per source line to render the live preview: md_class returns the CSS block
 * class for a line, md_text the text to display. Splitting the note body into
 * lines is done in candela; the per-line markdown parsing runs here in C. This
 * is the notes app showcase: candela calling a bundled C library on the host.
 *
 * The parser is single-line (no fenced-code or cross-line state): headings
 * (#, ##, ### and deeper), unordered list items (-, *, + followed by a space),
 * thematic breaks (three or more of -, * or _), indented code (a tab or four
 * leading spaces), and everything else as a paragraph. It covers the note
 * content shipped with the app.
 *
 * Both readers return a pointer the candela runtime copies immediately, so the
 * single reused static buffer is safe: md_class's result is copied before
 * md_text runs, and each call overwrites the buffer for the next copy.
 *
 * Build (Linux):   cc -shared -fPIC -O2 -o libmd.so md.c
 * Build (macOS):   cc -shared -fPIC -O2 -o libmd.dylib md.c
 * The library sits next to main.cdl; candela resolves a bare `dylib "md"` to
 * libmd.so / libmd.dylib in the app directory.
 */

#include <stdio.h>
#include <string.h>

#if defined(_WIN32)
#define EXPORT __declspec(dllexport)
#else
#define EXPORT
#endif

static char out_buf[8192];

static const char *skip_spaces(const char *s) {
    while (*s == ' ' || *s == '\t') {
        s++;
    }
    return s;
}

/* Leading run of '#', 1..6, that forms a heading (followed by a space or the
 * end of the line). 0 if the line is not a heading. */
static int heading_level(const char *s) {
    int n = 0;
    while (s[n] == '#') {
        n++;
    }
    if (n >= 1 && n <= 6 && (s[n] == ' ' || s[n] == '\0')) {
        return n;
    }
    return 0;
}

/* A thematic break: three or more of the same marker (-, * or _), spaces
 * allowed between them and nothing else on the line. */
static int is_thematic_break(const char *s) {
    char marker = 0;
    int count = 0;
    for (const char *p = s; *p; p++) {
        if (*p == ' ' || *p == '\t') {
            continue;
        }
        if (*p == '-' || *p == '*' || *p == '_') {
            if (marker == 0) {
                marker = *p;
            }
            if (*p != marker) {
                return 0;
            }
            count++;
        } else {
            return 0;
        }
    }
    return count >= 3;
}

static int is_bullet(const char *s) {
    return (s[0] == '-' || s[0] == '*' || s[0] == '+') && s[1] == ' ';
}

static int is_indented_code(const char *line) {
    if (line[0] == '\t') {
        return 1;
    }
    return line[0] == ' ' && line[1] == ' ' && line[2] == ' ' && line[3] == ' ';
}

/* Classify one markdown source line into a CSS block class. A blank line maps
 * to "blk-blank", which the caller skips. */
EXPORT const char *md_class(const char *line) {
    if (line == NULL) {
        return "blk-blank";
    }
    const char *s = skip_spaces(line);
    if (*s == '\0') {
        return "blk-blank";
    }
    if (is_indented_code(line)) {
        return "blk-code";
    }
    if (is_thematic_break(line)) {
        return "blk-hr";
    }
    int h = heading_level(s);
    if (h == 1) {
        return "blk-h1";
    }
    if (h == 2) {
        return "blk-h2";
    }
    if (h >= 3) {
        return "blk-h3";
    }
    if (is_bullet(s)) {
        return "blk-li";
    }
    return "blk-p";
}

/* Render one line to its display text: strip the heading markers, prefix a
 * bullet on list items, drop the marker on a rule, and dedent code. */
EXPORT const char *md_text(const char *line) {
    if (line == NULL) {
        out_buf[0] = '\0';
        return out_buf;
    }
    if (is_thematic_break(line)) {
        out_buf[0] = '\0';
        return out_buf;
    }
    if (is_indented_code(line)) {
        const char *body = line[0] == '\t' ? line + 1 : line + 4;
        snprintf(out_buf, sizeof(out_buf), "%s", body);
        return out_buf;
    }
    const char *s = skip_spaces(line);
    int h = heading_level(s);
    if (h > 0) {
        const char *title = skip_spaces(s + h);
        snprintf(out_buf, sizeof(out_buf), "%s", title);
        return out_buf;
    }
    if (is_bullet(s)) {
        const char *item = skip_spaces(s + 2);
        snprintf(out_buf, sizeof(out_buf), "-  %s", item);
        return out_buf;
    }
    snprintf(out_buf, sizeof(out_buf), "%s", s);
    return out_buf;
}
