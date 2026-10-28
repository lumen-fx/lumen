#!/usr/bin/env bash
# Drive the drag on one Wayland desktop, inside the image the Dockerfile
# beside this builds: wayland.sh sway|kwin|mutter LUMENC OUT_DIR
set -euo pipefail
desktop=$1 lumenc=$2 out=$3
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
work=$(mktemp -d)
export XDG_RUNTIME_DIR=$work/run
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
unset DISPLAY

build() {
  local xml=$1 name=$2 src=$3
  wayland-scanner client-header "$xml" "$work/$name-client.h"
  wayland-scanner private-code "$xml" "$work/$name.c"
  gcc -O1 -I"$work" -o "$work/$(basename "$src" .c)" "$src" "$work/$name.c" $(pkg-config --cflags --libs wayland-client)
}

case "$desktop" in
  sway)
    build /usr/share/wlr-protocols/unstable/wlr-virtual-pointer-unstable-v1.xml wlr-virtual-pointer "$here/wlr-pointer.c"
    cat > "$work/sway.conf" <<'CONF'
output HEADLESS-1 resolution 1280x1024 position 0 0
for_window [title=".*"] floating enable
default_floating_border none
CONF
    WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
      sway -c "$work/sway.conf" > "$out/compositor.log" 2>&1 &
    comp=$!
    for _ in $(seq 1 100); do
      [ -S "$XDG_RUNTIME_DIR/wayland-1" ] && ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock > /dev/null 2>&1 && break
      sleep 0.1
    done
    export WAYLAND_DISPLAY=wayland-1 SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock)
    status=0
    python3 "$here/drive.py" "$lumenc" "$out" --desktop wayland --compositor sway \
      --pointer "$work/wlr-pointer" 1280 1024 || status=$?
    ;;
  kwin)
    build /usr/share/plasma-wayland-protocols/fake-input.xml fake-input "$here/kwin-pointer.c"
    export KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
    dbus-run-session -- kwin_wayland --virtual --width 1280 --height 1024 --socket wayland-kwin \
      --no-lockscreen --exit-with-session \
      "env WAYLAND_DISPLAY=wayland-kwin python3 $here/drive.py $lumenc $out --desktop wayland --compositor kwin --pointer $work/kwin-pointer" \
      > "$out/compositor.log" 2>&1 || status=$?
    status=${status:-0}
    comp=
    ;;
  mutter)
    dbus-run-session -- bash -c "
      mutter --headless --wayland --no-x11 --virtual-monitor 1280x1024 --wayland-display wayland-mutter > '$out/compositor.log' 2>&1 &
      for _ in \$(seq 1 100); do [ -S '$XDG_RUNTIME_DIR/wayland-mutter' ] && break; sleep 0.1; done
      sleep 1
      WAYLAND_DISPLAY=wayland-mutter python3 '$here/drive.py' '$lumenc' '$out' --desktop wayland --compositor mutter --pointer python3 '$here/mutter-pointer.py'
    " || status=$?
    status=${status:-0}
    comp=
    ;;
  *) echo "wayland.sh: no desktop called $desktop" >&2; exit 2 ;;
esac
[ -n "${comp:-}" ] && kill "$comp" 2>/dev/null || true
cat "$out/app.log" || true
exit "${status:-1}"
