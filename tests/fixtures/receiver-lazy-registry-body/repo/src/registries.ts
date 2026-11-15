// One lazy registry per form: each entry is a thunk that imports its
// module when called.
export const regA = { a: () => import("./svc/a") } as const;
export const regB = { b: () => import("./svc/b") } as const;
export const regC = { c: () => import("./svc/c") } as const;
export const regD = { d: () => import("./svc/d") } as const;
