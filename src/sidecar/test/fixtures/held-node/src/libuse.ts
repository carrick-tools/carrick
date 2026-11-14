// Uses library members only the default lib set for target ES2022 and DOM provides.
export const last = [1, 2].at(-1);
export const has = Object.hasOwn({ a: 1 }, "a");
export const err = new Error("e", { cause: 1 });
export const el: HTMLElement | null = typeof document === "undefined" ? null : document.body;
