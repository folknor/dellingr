-- Character classes are fixed ASCII in dellingr. The differential gate runs
-- reference Lua in the C locale, as required by docs/compatibility.md.
-- Format's %c builds every byte without depending on string.char.
local letters = "aAcCdDgGlLpPsSuUwWxXzZ"
for i = 1, #letters do
    local letter = string.sub(letters, i, i)
    local pattern = "^%" .. letter .. "$"
    local members = {}
    for byte = 0, 255 do
        local subject = string.format("%c", byte)
        if string.find(subject, pattern) then
            members[#members + 1] = string.format("%02x", byte)
        end
    end
    print(letter .. "=" .. table.concat(members, ","))
end

-- Find's automatic literal search also accepts a closing capture byte.
local excluded = "^$*+?.([%-"
for byte = 0, 255 do
    local subject = string.format("%c", byte)
    if not string.find(excluded, subject, 1, true) then
        local first, last = string.find(subject, subject)
        local plain_first, plain_last = string.find(subject, subject, 1, true)
        if first ~= plain_first or last ~= plain_last then
            error("automatic literal search disagrees with plain search")
        end
        print(string.format("literal %02x=%d,%d", byte, first, last))
    end
end
