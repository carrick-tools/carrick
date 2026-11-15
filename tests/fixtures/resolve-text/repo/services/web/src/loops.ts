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

// A key read off an object element (carrick#4266). Read: each element's
// own entry, two keys of one head together, a key under a destructured key
// or a tuple position, and one head name reused by a second loop.
declare function p_member(name: string): void;
declare function p_member_pair(name: string): void;
declare function p_member_held(name: string): void;
declare function p_member_tuple(name: string): void;
declare function p_member_again(name: string): void;
declare function p_member_computed(name: string): void;
declare function p_member_computed_key(name: string): void;
declare function p_member_dynamic(name: string): void;
declare function p_member_missing(name: string): void;
declare function p_member_handed(name: string): void;
declare function p_member_written(name: string): void;
declare function p_member_reached(name: string): void;
declare function p_member_exported(name: string): void;
declare function touch(entry: { path: string }): void;
declare const dyn: string;

const ROUTES = [
  { path: '/a', verb: 'get' },
  { path: '/b', verb: 'post' },
];
for (const r of ROUTES) p_member(r.path);

for (const r of [{ verb: 'get', path: '/x' }, { verb: 'post', path: '/y' }]) p_member_pair(`${r.verb} ${r.path}`);

for (const { route } of [{ route: { path: '/h1' } }, { route: { path: '/h2' } }]) p_member_held(route.path);

for (const [, r] of [['a', { path: '/t1' }], ['b', { path: '/t2' }]] as const) p_member_tuple(r.path);

for (const r of [{ path: '/again' }]) p_member_again(r.path);

// Not read: an element the code does not state whole, a key no element
// writes plainly, a key read by a computed name, or an element something
// may change.
for (const r of [{ path: '/c1' }, { path: make()[0] }]) p_member_computed(r.path);

for (const r of [{ [dyn]: '/k' }]) p_member_computed_key(r.path);

for (const r of [{ path: '/d' }]) p_member_dynamic(r[dyn as 'path']);

for (const r of [{ path: '/m1' }, { other: '/m2' }]) p_member_missing(r.path);

for (const r of [{ path: '/e' }]) {
  touch(r);
  p_member_handed(r.path);
}

for (const r of [{ path: '/w' }]) {
  r.path = '/z';
  p_member_written(r.path);
}

const REACHED = [{ path: '/r' }];
touch(REACHED[0]);
for (const r of REACHED) p_member_reached(r.path);

export const SHARED = [{ path: '/s1' }, { path: '/s2' }] as const;
for (const r of SHARED) p_member_exported(r.path);
