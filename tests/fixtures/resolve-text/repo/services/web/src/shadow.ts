export {};

// An inner scope that declares the same name does not block the outer one.
const OUTER = { a: "/outer" };

export function inner(): string {
  const OUTER = { a: "/inner" };
  OUTER.a = "/inner2";
  return OUTER.a;
}

const p_shadow = OUTER.a;
