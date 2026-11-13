// A function's own `let` written again (carrick#3679 S8): which writes reach
// each read, in the order the body runs. Each probe is a call of a `p_*`
// function whose argument is the local a reader asks about, so a test finds
// a probe by its callee. Every literal is annotated `string`: the compiler
// types an unannotated `let` as `string` anyway, and the text reader reads a
// one-literal type before the binding table.
export {};

declare const f: boolean;
declare const xs: string[];

declare function p_after(name: string): void;
declare function p_exit(name: string): void;
declare function p_both(name: string): void;
declare function p_looped(name: string): void;
declare function p_nullish(name: string | undefined): void;
declare function p_hidden(name: string): void;

// A write after the read is not what the read holds.
export function after(): void {
  let p: string = "/before";
  p_after(p);
  p = "/after";
}

// A branch that writes and returns passes nothing on.
export function exit(): void {
  let p: string = "/a";
  if (f) {
    p = "/b";
    return;
  }
  p_exit(p);
}

// Either branch may run: both values reach the read.
export function both(): void {
  let p: string = "/a";
  if (f) p = "/b";
  p_both(p);
}

// A write in a loop: the flow refuses the local, and the set stands.
export function looped(): void {
  let p: string = "/a";
  for (const x of xs) p = x;
  p_looped(p);
}

// `??=` adds a way and keeps the one that reaches it. Unconditional, so a
// flow that let it replace the ways would read `/b` alone.
export function nullish(): void {
  let p: string | undefined = "/a";
  p ??= "/b";
  p_nullish(p);
}

// A write in an `if` test: the flow refuses the local, and the set stands.
export function hidden(): void {
  let p: string = "/a";
  if ((p = "/b")) {
  }
  p_hidden(p);
}
