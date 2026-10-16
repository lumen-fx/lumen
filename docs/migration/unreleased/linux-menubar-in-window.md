# `<menubar>` is drawn in the window on Linux

This affects apps that declare a `<menubar>` and run on Linux. It used to show
nothing there, because Linux desktops have no menu bar for an app to join. The
same markup now becomes a bar at the top of the window, its items reach
`on_menu`, and their `accel` shortcuts work. macOS and Windows keep the OS
menu bar.

An app that built its own in-window bar for Linux would now show two. Remove
the hand-built one, or drop the `<menubar>` on Linux:

```html
<!-- before: a hand-built bar beside the declared one -->
<menubar>...</menubar>
<row class="my-linux-menubar">...</row>
<!-- after -->
<menubar>...</menubar>
```

The root's top padding grows by the bar's height, so the content starts below
the bar. A script reading the root's computed `padding-top` sees that sum.
