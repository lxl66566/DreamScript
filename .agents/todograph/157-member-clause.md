---
id: 157-member-clause
description: Member calls gain per-site solved-clause shapes so a member's own generic clause keys its instances (unblocks the fold/zip member move, item 157)
status: done
priority: 1
type: task
tags: [M6, S, sequenced]
assignee: lane-mclause
created_at: 2026-11-27T23:14:14.766312061Z
updated_at: 2026-11-28T00:52:27.382206007Z
done_at: 2026-11-28T00:52:27.382206007Z
---
The member-clause seam (opened 2026-11-28 from lane157b's structural analysis): a member's own generic clause over a bodied member refuses DS4009 at the instance key because the member substitution binds only the impl head's variables (`member_args` head-vs-receiver, `GenericMember.generics` = impl generics), the checker records no per-site solved-clause shape the plan's discovery could bind (member calls record no `generic_calls`), and the instance/site keys `(impl_site, member, recv)` carry no clause dimensions — two same-receiver sites with different clause solutions could not be distinguished. The seam: the checker records the call site's solved clause shape, the plan-side discovery walks it like a free generic call's arguments, and the instance/site keys widen with the clause dimensions; then the fold/zip member move (todograph 157) lands over it, and a bodied member's own generic clause instantiates per shape like a free fn's.
