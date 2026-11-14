// Names a `for…of` head binds (carrick#2093). Each probe is a call of a `p_*`
// function whose argument is the name a reader asks about, so a test finds a
// probe by its callee.
export {};

declare function make(): string[];
declare function hand(list: string[]): void;

declare function p_each(name: string): void;
declare function p_tuple(name: string): void;
declare function p_keyed(name: string): void;
declare function p_two(name: string): void;
declare function p_frozen(name: string): void;
declare function p_local(name: string): void;
declare function p_loose(name: string): void;
declare function p_pushed(name: string): void;
declare function p_handed(name: string): void;
declare function p_let_list(name: string): void;
declare function p_spread(name: string): void;
declare function p_call(name: string): void;
declare function p_param(name: string): void;
declare function p_let_head(name: string): void;
declare function p_in(name: string): void;
declare function p_default(name: string): void;

// Read: the code states each element and nothing changes the list.
const KINDS = ['small', 'large'];
export const FROZEN = ['create', 'apply'] as const;

for (const kind of KINDS) p_each(kind);

for (const [file, type] of [['f1', 't1'], ['f2', 't2']] as const) p_tuple(`${file}:${type}`);

for (const { name } of [{ name: 'n1' }, { name: 'n2' }]) p_keyed(name);

for (const a of ['a1', 'a2']) for (const b of ['b1', 'b2']) p_two(`${a}${b}`);

for (const action of FROZEN) p_frozen(action);

export function local(): void {
  const STEPS = ['one', 'two'];
  for (const step of STEPS) p_local(step);
}

// Not read: the list may change, or the code does not state it.
export const LOOSE = ['a', 'b'];
for (const loose of LOOSE) p_loose(loose);

const PUSHED = ['a'];
PUSHED.push('b');
for (const pushed of PUSHED) p_pushed(pushed);

const HANDED = ['a'];
hand(HANDED);
for (const handed of HANDED) p_handed(handed);

let LET_LIST = ['a'];
for (const fromLet of LET_LIST) p_let_list(fromLet);

const SPREAD_BASE = ['x', 'y'];
const SPREAD = [...SPREAD_BASE, 'z'];
for (const spread of SPREAD) p_spread(spread);

for (const called of make()) p_call(called);

export function param(list: string[]): void {
  for (const fromParam of list) p_param(fromParam);
}

for (let letHead of KINDS) p_let_head(letHead);

for (const key in { a: 1 }) p_in(key);

for (const [withDefault = 'z'] of [['a']]) p_default(withDefault);
