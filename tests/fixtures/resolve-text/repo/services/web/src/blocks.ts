// Declarations written below a function's top level, and in functions written
// inside a body (carrick#3794). Each probe is a call of a `p_*` function whose
// argument is the name a reader asks about, so a test finds a probe by its
// callee. Every literal is annotated `string`: the compiler types an
// unannotated `const x = "lit"` as its literal, and the text reader reads that
// type before the binding table.
export {};

declare const flag: boolean;
declare const k: string;
declare const process: { env: Record<string, string | undefined> };
declare function handler(path: string, fn: () => void): void;

declare function p_if(name: string): void;
declare function p_try(name: string): void;
declare function p_catch(name: string): void;
declare function p_case(name: string): void;
declare function p_bare(name: string): void;
declare function p_loop(name: string): void;
declare function p_loop_body(name: string): void;
declare function p_module_block(name: string): void;
declare function p_callback(name: string): void;
declare function p_callback_block(name: string): void;
declare function p_handler(name: string): void;
declare function p_inner(name: string): void;
declare function p_moved(name: string): void;
declare function p_nested_moved(name: string): void;
declare function p_shadow(name: string): void;
declare function p_shadow_outer(name: string): void;
declare function p_pair_a(name: string): void;
declare function p_pair_b(name: string): void;

const BASE = "https://example.test";

export function blocks(): void {
  if (flag) {
    const inIf: string = "order.created";
    p_if(inIf);
  }
  try {
    const inTry: string = `${BASE}/try`;
    p_try(inTry);
  } catch (err: any) {
    p_catch(err);
  }
  switch (k) {
    case "a": {
      const inCase: string = "case.a";
      p_case(inCase);
    }
  }
  {
    const bare: string = "bare";
    p_bare(bare);
  }
  for (const item of ["x"]) {
    p_loop(item);
  }
  for (const item of ["x"]) {
    const inLoop: string = "loop.body";
    p_loop_body(inLoop);
  }
  [1].forEach(() => {
    const inCallback: string = "cb.top";
    p_callback(inCallback);
    if (flag) {
      const deeper: string = "cb.block";
      p_callback_block(deeper);
    }
    function innerInCallback(): string {
      return "cb.inner";
    }
    innerInCallback();
  });
  function inner(): string {
    return "inner.ret";
  }
  p_inner(inner());
  if (flag) {
    let moved: string = "a";
    moved = "b";
    p_moved(moved);
  }
  [1].forEach(() => {
    let nestedMoved: string = "a";
    nestedMoved = "b";
    p_nested_moved(nestedMoved);
  });
  const same: string = "outer";
  if (flag) {
    const same: string = "inner";
    p_shadow(same);
  }
  p_shadow_outer(same);
  [1].map((n = 2) => n);
}

if (process.env.X) {
  const atModule: string = "module.block";
  p_module_block(atModule);
}

// Two blocks, one line, one name: no row says which a use means. At module
// scope the write census keys by binding, so both are entries. (Inside a body
// the second declaration would be a shadow and neither would be.)
{ const pair: string = "x"; p_pair_a(pair); } { const pair: string = "y"; p_pair_b(pair); }

handler("/x", () => {
  const inHandler: string = "handler.top";
  p_handler(inHandler);
});
