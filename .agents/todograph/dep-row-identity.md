---
id: dep-row-identity
description: "Dep rows carry the transitive bundle identity: re-key dep caches, the list writer, cache validation, and watcher hot-swap (ruled 2026-11-28)"
status: open
priority: 2
type: task
created_at: 2026-11-28T00:43:13.374625805Z
updated_at: 2026-11-28T02:50:19.650789806Z
---
Ruled 2026-11-28 by the author: option A — dep rows for non-@std deps will carry the transitive bundle identity (the manifest hash's own principle, identity flows transitively), so a transitive-only repin re-keys the bare walk's demand and the walk refuses loudly instead of serving the first-compiled world silently. Not scheduled for implementation yet: the ruling is recorded, the work queues on a later round. Implementation scope when picked: re-keys the dep caches; touches the dep-list writer's recorded identities (identities_of), the dep-cache validation (lookup_dep), and the watcher's hot-swap semantics. The bare-walk transitive-repin gap stays pinned by tests and documented in design-decisions.md's Package stack entry until then.