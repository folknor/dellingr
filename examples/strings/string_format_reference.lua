print("integer=" .. string.format("%d|%i|%u|%o|%x|%X", 42, -42, 42, 10, 255, 255))
print("unsigned=" .. string.format("%u|%x", -1, -1))

print("decimal=" .. string.format(
    "%.2e|%.2E|%.2f|%.4g|%.4G", 12.5, 12.5, 3.14159, 12, 0.000012))
print("decimal flags=" .. string.format("[% 6.2f]|[%#.4g]", 1.5, 12))
print("decimal infinity=" .. string.format("[%010f]", math.huge))

print("hex=" .. string.format("%a|%A", 1.5, 1.5))
print("hex q=" .. string.format("%q", 1.5))
print("hex precision=" .. string.format("[%#.0a]|[%.0a]", 1.5, 1.5))

print("width=" .. string.format("[%+06d]|[%-6d]|[%#08x]", 42, 42, 42))
print("string width=" .. string.format("[%5.2s]", "hello"))
print("precision=" .. string.format("[%#.3o]|[%08.5x]|[%+08.5d]", 1, 42, 42))
