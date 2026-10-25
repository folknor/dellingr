-- DIFF: lone_separators - table constructors accept a leading or lone comma or
-- semicolon separator; Lua 5.2 and 5.4 reject these forms at parse time. See
-- docs/compatibility.md.
-- EXPECT: lone semicolon builds an empty table: true
-- EXPECT: lone comma builds an empty table: true
-- EXPECT: leading semicolon is skipped: true
-- EXPECT: leading comma is skipped: true

local lone_semicolon = {;}
print("lone semicolon builds an empty table: " .. tostring(#lone_semicolon == 0 and next(lone_semicolon) == nil))

local lone_comma = {,}
print("lone comma builds an empty table: " .. tostring(#lone_comma == 0 and next(lone_comma) == nil))

local leading_semicolon = {; 1, 2}
print("leading semicolon is skipped: " .. tostring(#leading_semicolon == 2 and leading_semicolon[1] == 1))

local leading_comma = {, x = 1}
print("leading comma is skipped: " .. tostring(leading_comma.x == 1 and #leading_comma == 0))
