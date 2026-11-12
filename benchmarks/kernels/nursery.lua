-- Kernel: the fiber path — spawn/join nursery cycles. Mirrors nursery.ds
-- exactly with coroutines: each cycle creates the two child coroutines
-- and resumes each to completion (create is the spawn allocation,
-- resume-to-dead is the join drive); every 100th cycle a child of a
-- child nests a second create/resume pair (the interlock's
-- join-across-levels shape). The final walk re-reads the shared table
-- to checksum the children's answers.

local N = 200000
local LABEL = "fiber-probe"

local function child_a(out, i)
  out[#out + 1] = (i % 97) + (i % 13)
end

local function child_b(out, i, label)
  out[#out + 1] = #label + (i % 11)
end

local function deep(i, label)
  local sub = {}

  local function grandchild()
    sub[#sub + 1] = #label + (i % 7)
  end

  local function child()
    local co = coroutine.create(grandchild)
    coroutine.resume(co)
  end

  local co = coroutine.create(child)
  coroutine.resume(co)
  if #sub > 0 then
    return sub[1]
  end
  return 0
end

local out = {}
local deep_sum = 0
for i = 0, N - 1 do
  local a = coroutine.create(child_a)
  coroutine.resume(a, out, i)
  local b = coroutine.create(child_b)
  coroutine.resume(b, out, i, LABEL)
  if i % 100 == 99 then
    deep_sum = deep_sum + deep(i, LABEL)
  end
end

local s = 0
for _, v in ipairs(out) do
  s = s + v
end
print(string.format("result: %d %d %d", s, deep_sum, #out))
