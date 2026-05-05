-- String pattern kernel.
-- Exercises lua-patterns across find/match/gsub/gmatch, with patterns
-- that cover character classes, lazy/greedy quantifiers, optional
-- captures, and backtracking - closer to real log/URL parsing.

local log = "[2026-05-05 12:34:56] WARN  request_id=req-7Fa2 method=GET status=200 path=/api/v1/users size=1532b"
local url = "https://user:pass@example.com:8443/path/to/resource?q=hello+world&page=42#frag-1"
local csv = '"name","age","city"\n"alice",30,stockholm\n"bob, jr.",25,"oslo"'

-- Real-world-shaped utility: literal substring replace built out of
-- pattern escaping + plain gsub. Triple-gsub per call.
local function replace(str, what, with)
    what = string.gsub(what, "[%(%)%.%+%-%*%?%[%]%^%$%%]", "%%%1")
    with = string.gsub(with, "[%%]", "%%%%")
    return string.gsub(str, what, with)
end

-- s:match()-style trim: extends the string lib so the method form works.
-- Exercises both the string-method dispatch path and a position-capture
-- (`()`) followed by a content-capture branch.
function string.trim(self)
    return self:match("^()%s*$") and "" or self:match("^%s*(.*%S)")
end

local padded = "    leading and trailing whitespace      "

-- Method-chained currency formatter: tostring -> reverse -> gsub ->
-- reverse -> gsub. Five method-call dispatches per invocation, plus
-- the gsub passes themselves.
local function format_valuta(i)
    return (tostring(i):reverse():gsub("%d%d%d", "%1."):reverse():gsub("^,", ""))
end

function _bench()
    local total = 0

    -- log line: pattern with class union, lazy (-), optional ?, and *
    -- ([%s%#%p]-(%a?%d*)%S*) plus the realistic-shape variant
    local _, _, ts, level = string.find(log, "%[([%d%-%s:]+)%]%s+(%u+)")
    total = total + (ts and #ts or 0) + (level and #level or 0)
    for m in string.gmatch(log, "[%s%#%p]-(%a?%d*)%S*") do
        total = total + #m
    end

    -- url: greedy + lazy mix, optional userinfo, optional port, optional query
    -- scheme://[user[:pass]@]host[:port]/path[?query][#frag]
    local scheme, userinfo, host, port, path, query, frag = string.match(
        url,
        "^(%w+)://([^@/]*)@?([^:/]+):?(%d*)([^?#]*)%??([^#]*)#?(.*)$"
    )
    total = total
        + #(scheme or "") + #(userinfo or "") + #(host or "") + #(port or "")
        + #(path or "") + #(query or "") + #(frag or "")

    -- csv-ish: gmatch over fields, alternating quoted vs bare
    for field in string.gmatch(csv, '"([^"]*)"') do
        total = total + #field
    end

    -- gsub with function replacement: rewrite numbers as <NUM>
    local out, n = string.gsub(log, "%d+", "<NUM>")
    total = total + #out + n

    -- gsub with backref: kv-flip on the log's key=value pairs
    local flipped, k = string.gsub(log, "(%w+)=([^%s]+)", "%2=%1")
    total = total + #flipped + k

    -- literal replace via escaped-pattern gsub: 3 gsubs per call
    local r = replace(log, "request_id=req-7Fa2", "request_id=ANON")
    total = total + #r

    -- string-method dispatch: trim() and format_valuta() chains
    total = total + #padded:trim()
    total = total + #format_valuta(1234567890)

    return total
end

for i = 1, 1500 do _bench() end
print("strings/patterns: true")
