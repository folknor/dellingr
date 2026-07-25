-- Test comment handling

-- Single line comment
print("Test 1: single line comment passed")

--[[ Multi-line comment
This spans multiple lines
and should be ignored
]]
print("Test 2: multi-line comment passed")

--[[
Even with blank lines

inside
]]
print("Test 3: multi-line with blanks passed")

local x = 5 --[[ inline multi-line ]] + 3
print("Test 4: inline multi-line: " .. tostring(x == 8))

-- Nested brackets in comment shouldn't break anything
--[[ This has [brackets] inside ]]
print("Test 5: brackets in comment passed")

local leveled_comment_ran = false
--[=[
leveled_comment_ran = true
print("This must stay commented")
]=]
print("Test 6: leveled comment: " .. tostring(not leveled_comment_ran))

print("All comment tests passed!")
