/**
 * carrick#1444 — an alias whose own type the stub project could not resolve is
 * not an answer, whatever the compiler prints for it.
 *
 * `resolve_definitions` runs over the emitted declaration tree alone, with no
 * dependencies installed (the stub is written to a temp dir, so nothing
 * resolves out of it). A reference that leaves the tree therefore lands on
 * TypeScript's unresolved-reference placeholder — `TypeFlags.Any` carrying the
 * internal `intrinsicName === 'error'` — and the compiler prints that
 * placeholder as the REFERENCE TEXT it failed to resolve, not as `any`. An
 * instantiation over a dependency's internal generics reads as a confident
 * name with no members in it, and the publication rule that refuses an empty
 * answer (`text_is_bare_top_type` on the scanner side) never fires, so the
 * operation is counted typed and the index hands a reader a name that resolves
 * nowhere.
 *
 * At a MEMBER position the same echo is worth keeping — `status:
 * EngagementStatus` names something in the producer's own repo — so this is
 * scoped to the alias's own type, which is the whole answer for that
 * operation.
 */
export {};
