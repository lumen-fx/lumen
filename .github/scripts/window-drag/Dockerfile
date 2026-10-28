# The Wayland desktops the drag is driven on: sway (wlroots, no toplevel
# drag), KWin and mutter (both with xdg_toplevel_drag_v1), as current
# releases ship them. The lumenc under test is built on the runner and
# mounted in; gtk3 and alsa-lib are what it links.
FROM archlinux:latest
RUN pacman -Syu --noconfirm --needed \
      sway wtype kwin mutter \
      plasma-wayland-protocols wlr-protocols wayland wayland-protocols \
      libxkbcommon gtk3 alsa-lib gcc pkgconf python python-dbus dbus \
      ttf-dejavu \
 && pacman -Scc --noconfirm \
 && useradd -u 1001 -m drag
USER drag
