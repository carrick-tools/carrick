export {};

declare const maybe: string | undefined;
declare const flag: boolean;
declare const union: "/u/a" | "/u/b";
const BASE: string = "/api";

const p_coalesce = maybe ?? "/fallback";
const p_or = maybe || "/or";
const p_cond = flag ? BASE : "/other";
const p_chain = maybe ?? BASE ?? "/last";
const p_union = flag ? union : "/u/c";

// carrick#3835: a `??` after a name typed as one non-nullable text is never taken.
const p_nullish_typed = BASE ?? "/never";
const p_or_typed = BASE || "/falsy";
