-- brokkr verdict workload derived from examples/strings/patterns.lua (see
-- that file for the probe rationale). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local log = "[2026-05-05 12:34:56] WARN  request_id=req-7Fa2 method=GET status=200 path=/api/v1/users size=1532b"
local url = "https://user:pass@example.com:8443/path/to/resource?q=hello+world&page=42#frag-1"
local csv = '"name","age","city"\n"alice",30,stockholm\n"bob, jr.",25,"oslo"'

-- Literal substring replace built out of pattern escaping + plain gsub.
local function replace(str, what, with)
    what = string.gsub(what, "[%(%)%.%+%-%*%?%[%]%^%$%%]", "%%%1")
    with = string.gsub(with, "[%%]", "%%%%")
    return string.gsub(str, what, with)
end

-- s:match()-style trim: extends the string lib so the method form works.
function string.trim(self)
    return self:match("^()%s*$") and "" or self:match("^%s*(.*%S)")
end

local padded = "    leading and trailing whitespace      "

-- Method-chained currency formatter.
local function format_valuta(i)
    return (tostring(i):reverse():gsub("%d%d%d", "%1."):reverse():gsub("^,", ""))
end

local function kernel()
    local total = 0

    local _, _, ts, level = string.find(log, "%[([%d%-%s:]+)%]%s+(%u+)")
    total = total + (ts and #ts or 0) + (level and #level or 0)
    for m in string.gmatch(log, "[%s%#%p]-(%a?%d*)%S*") do
        total = total + #m
    end

    local scheme, userinfo, host, port, path, query, frag = string.match(
        url,
        "^(%w+)://([^@/]*)@?([^:/]+):?(%d*)([^?#]*)%??([^#]*)#?(.*)$"
    )
    total = total
        + #(scheme or "") + #(userinfo or "") + #(host or "") + #(port or "")
        + #(path or "") + #(query or "") + #(frag or "")

    for field in string.gmatch(csv, '"([^"]*)"') do
        total = total + #field
    end

    local out, n = string.gsub(log, "%d+", "<NUM>")
    total = total + #out + n

    local flipped, k = string.gsub(log, "(%w+)=([^%s]+)", "%2=%1")
    total = total + #flipped + k

    local r = replace(log, "request_id=req-7Fa2", "request_id=ANON")
    total = total + #r

    total = total + #padded:trim()
    total = total + #format_valuta(1234567890)

    return total
end

local K = 2300

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/patterns: true")
