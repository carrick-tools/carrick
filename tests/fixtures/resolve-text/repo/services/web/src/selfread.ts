export {};

// A constant that reads itself is a hole, never a loop.
// @ts-expect-error: used before its declaration, on purpose.
const p_self: string = `${p_self}/x`;
