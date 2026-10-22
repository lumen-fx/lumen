# Window messages name the window

This affects Rust code that builds or matches the window messages in
`lumen_core::input` and `lumen_core::window`, or drives the close path.

- `CloseRequest`, `WindowFocused`, and `WindowOccluded` gained a `window`
  field. A veto answers for the window it was asked about:

  ```rust
  msgs.write(CloseRequest { reason: request.reason, vetoed: true });
  msgs.write(CloseRequest { window: request.window, reason: request.reason, vetoed: true });
  ```

- `WindowRequest::Close` takes the window to close:
  `WindowRequest::Close(window)`.
- `CloseRequests::take` returns a `CloseAsk` (`Quit`, or `Windows` naming the
  windows to close) rather than a `CloseReason`. `run_close_request` takes the
  window being closed, `None` for every window, and returns a `CloseOutcome`
  (`Vetoed`, `Closed`, `Quit`, or `Hidden`) rather than a `bool`.
- `PointerState` gained `window`: the window the pointer is over, `None` for
  the primary one.

A closing window now sends `WindowClosing` a tick before it goes, so whatever
put content in it can take that content back.
