-- Counter demo, ported from apps/counter's inline Rhai `<script>` to Lua.
-- Same engine-function surface: signal / derive / on, plus the click
-- lifecycle handlers the host-generic runtime dispatches.

function bump(by)
    local clicks = signal("clicks", 0)
    clicks:set(clicks:get() + by)
end

-- Per-id callback router: a click on the element with id "reset" routes
-- here instead of the global on_click.
function handle_reset_click(id)
    signal("clicks", 0):set(0)
end

function on_start()
    local clicks = signal("clicks", 0)
    -- Computed signal: counter_label re-derives whenever clicks changes.
    derive("counter_label", { clicks }, function(n)
        return "Lumen - clicks: " .. n
    end)
    on("click", "reset", "handle_reset_click")
end

function on_click(id)
    bump(1)
end

function on_double_click(id)
    bump(9)
end

function on_long_press(id)
    signal("clicks", 0):set(0)
end
