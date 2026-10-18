# A signal written after the bindings ran paints on the next tick

This affects plugins and Rust systems that read the `PropertyStore` dirty
queue.

A signal written by a system the schedule ran after the `bind-*` readers used
to be cleared from the dirty queue at the end of its tick unseen, so the screen
kept the old value until some later input. Such a write now stays in the queue
into the next tick, which the run loop schedules at once, and the readers apply
it there. A DOM mutation queued from the C ABI or an SDK after the tick's DOM
commands were collected likewise schedules the tick that applies it.

A system that gates on `dirty_peek` can therefore see a key on the tick after
it was written as well as on the tick itself. A system that applies signal
writes and skips its work when none landed reads the queue with
`PropertyStore::observe_dirty` instead, so a write that lands after it is kept
for its next run.
