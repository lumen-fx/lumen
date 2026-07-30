-- ---------- helpers -------------------------------------------------
local function wmo_to_kind(code)
    if code <= 1                  then return "sun"   end
    if code <= 3                  then return "cloud" end
    if code >= 71 and code <= 77  then return "snow"  end
    if code >= 85 and code <= 86  then return "snow"  end
    if code >= 95                 then return "rain"  end
    return "rain"
end

local function pretty_kind(k)
    if k == "snow"  then return "Snow"        end
    if k == "rain"  then return "Light rain"  end
    if k == "cloud" then return "Cloudy"      end
    return "Sunny and clear"
end

local function day_label(idx)
    local names = { "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun" }
    return names[(idx % 7) + 1]
end

-- Truncate toward zero, matching Rhai's `to_int()`.
local function to_int(x)
    if x >= 0 then return math.floor(x) else return math.ceil(x) end
end

local function fmt_temp(c)
    local unit = signal("unit", "C"):get()
    if unit == "F" then
        local f = c * 9.0 / 5.0 + 32.0
        return "" .. to_int(f) .. "\u{00B0}"
    end
    return "" .. to_int(c) .. "\u{00B0}"
end

-- ---------- rendering ----------------------------------------------
local function render_hero()
    local city = signal("city", ""):get()
    if city == "" then
        set_text("hero-city", "Pick a city")
        set_text("hero-condition", "Tap the search box and press Enter")
        set_text("hero-temp", "\u{2014}\u{00B0}")
        set_text("hero-high", "H \u{2014}\u{00B0}")
        set_text("hero-low", "L \u{2014}\u{00B0}")
        return
    end
    local cond = signal("hero_label", "Loading\u{2026}"):get()
    local temp = signal("hero_temp", 0.0):get()
    local high = signal("hero_high", temp):get()
    local low  = signal("hero_low", temp):get()
    set_text("hero-city", city)
    set_text("hero-condition", cond)
    set_text("hero-temp", fmt_temp(temp))
    set_text("hero-high", "H " .. fmt_temp(high))
    set_text("hero-low", "L " .. fmt_temp(low))
    local kind = signal("hero_kind", "sun"):get()
    set_src("hero-icon", "icons/" .. kind .. ".png")
end

local function render_forecast()
    local codes = signal("daily_codes", {}):get()
    local highs = signal("daily_highs", {}):get()
    if #codes == 0 then return end
    local n = #codes
    if n > 7 then n = 7 end
    for i = 0, n - 1 do
        local kind = wmo_to_kind(codes[i + 1])
        set_text("day-" .. i .. "-name", day_label(i))
        set_text("day-" .. i .. "-temp", fmt_temp(highs[i + 1]))
        set_src("day-" .. i .. "-icon", "icons/" .. kind .. ".png")
    end
end

-- ---------- HTTP flow ----------------------------------------------
local function search_city(name)
    print("search_city: " .. name)
    set_text("status", "Fetching " .. name .. "\u{2026}")
    local url = "https://geocoding-api.open-meteo.com/v1/search?count=1&name=" .. name
    fetch(url, "geo")
end

local function fetch_forecast(lat, lon)
    set_text("status", "Fetching forecast\u{2026}")
    local url = "https://api.open-meteo.com/v1/forecast?current_weather=true&daily=temperature_2m_max,weathercode&timezone=auto&latitude="
        .. lat .. "&longitude=" .. lon
    fetch(url, "forecast")
end

-- ---------- event handlers -----------------------------------------
function on_start()
    signal("unit", "C"):set("C")
    render_hero()
end

function on_text_input(id, text)
    if id == "search" then
        search_city(text)
    end
end

function on_click(id)
    if id == "unit" then
        local unit = signal("unit", "C")
        if unit:get() == "C" then
            unit:set("F")
            set_text("unit", "\u{00B0}F")
        else
            unit:set("C")
            set_text("unit", "\u{00B0}C")
        end
        render_hero()
        render_forecast()
    end
end

function on_long_press(id)
    if id == "search" or id == "dropzone" then
        signal("city", ""):set("")
        set_text("search", "")
        set_text("status", "Reset.")
        render_hero()
    end
end

function on_file_dropped(id, path)
    if id == "dropzone" then
        set_text("status", "Imported: " .. path)
    end
end

function on_fetch(tag, body)
    local data = parse_json(body)
    if tag == "geo" then
        local results = data.results
        if type(results) ~= "table" or #results == 0 then
            set_text("status", "City not found.")
            return
        end
        local r = results[1]
        signal("city", ""):set(r.name)
        signal("hero_label", ""):set("Loading\u{2026}")
        fetch_forecast(r.latitude, r.longitude)
        return
    end
    if tag == "forecast" then
        local cur = data.current_weather
        local kind = wmo_to_kind(cur.weathercode)
        signal("hero_label", ""):set(pretty_kind(kind))
        signal("hero_kind", "sun"):set(kind)
        signal("hero_temp", 0.0):set(cur.temperature)
        local daily = data.daily
        signal("daily_codes", {}):set(daily.weathercode)
        signal("daily_highs", {}):set(daily.temperature_2m_max)
        set_text("status", "Updated.")
        render_hero()
        render_forecast()
    end
end

function on_fetch_error(tag, msg)
    set_text("status", "Fetch " .. tag .. " failed: " .. msg)
end
