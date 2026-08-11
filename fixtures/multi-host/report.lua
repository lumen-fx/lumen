-- The Lua half of a two-language app. `shared` is named as a string dep so
-- this program never seeds it; the value comes from model.cdl through the
-- signal bus.

function on_start()
    derive("seen_by_lua", { "shared" }, function(v)
        return tostring(v) .. "+lua"
    end)
end

function on_ready()
    signal("lua_ready", ""):set("1")
end
