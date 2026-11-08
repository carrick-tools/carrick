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
