/**
 * A request library as an installed package, for the tests that read a call's
 * result through a service's wrapper rules (carrick#1841, carrick#1843).
 *
 * Every wrapper it exports is a type alias, in the forms the language has for
 * one:
 *
 *  - `Task<A>`, an alias of a class. The type has the class's own symbol
 *    (`__Task`) and own arguments, and the alias's symbol and arguments
 *    beside them;
 *  - `Outcome<A, E>`, an alias of a union. A union has no symbol and no
 *    arguments of its own, so the alias states both;
 *  - `Reply<T>`, an alias of an object literal. Its own symbol is the
 *    compiler's anonymous `__type`, and its arguments are the alias's;
 *  - `Scoped<Scope, T>`, an alias of an interface whose parameters are not
 *    the interface's own: `Envelope<T>` has one argument, the alias two.
 *
 * `Envelope<T>` is an interface, the form a rule has always read.
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

export const WIRE_PACKAGE = 'tiny-wire';

/** The module globs a rule for this package carries. */
export const WIRE_ORIGIN = [WIRE_PACKAGE, `${WIRE_PACKAGE}/*`];

const WIRE_PACKAGE_DTS = `export declare class __Task<A> {
  then(func: (value: A) => void): this;
}
export type Task<A> = __Task<A>;

declare class __Outcome<A, E> {
  isDone(): boolean;
}
interface Done<A, E> extends __Outcome<A, E> {
  readonly tag: "Done";
  readonly value: A;
}
interface Failed<A, E> extends __Outcome<A, E> {
  readonly tag: "Failed";
  readonly error: E;
}
export type Outcome<A, E> = Done<A, E> | Failed<A, E>;

export type Maybe<A> = { present: true; value: A } | { present: false };

type BodyKinds = { text: string; json: unknown };

export type Reply<T> = {
  status: number;
  ok: boolean;
  response: Maybe<T>;
  url: string;
  headers: Record<string, string>;
};

export interface Envelope<T> {
  data: T;
  requestId: string;
}

export type Scoped<Scope extends string, T> = Envelope<T>;

export declare class WireError extends Error {
  url: string;
}

export declare const Wire: {
  make: <K extends keyof BodyKinds>(config: {
    url: string;
    method?: "GET" | "POST";
    type: K;
  }) => Task<Outcome<Reply<BodyKinds[K]>, WireError>>;
  enveloped: <T>(url: string) => Task<Outcome<Envelope<T>, WireError>>;
  typed: <T>(url: string) => Task<Outcome<Reply<T>, WireError>>;
  untyped: (url: string) => Task<Outcome<unknown, WireError>>;
  task: <T>(url: string) => Task<T>;
  outcome: <T>(url: string) => Outcome<T, WireError>;
  reply: <T>(url: string) => Reply<T>;
  scoped: <T>(url: string) => Scoped<"billing", T>;
};
`;

/**
 * A temp repo with the package installed under `node_modules` and the given
 * sources under `src/`. The caller removes `repoDir`.
 */
export function writeWireRepo(
  prefix: string,
  sources: Record<string, string>
): { repoDir: string; pathOf: (name: string) => string } {
  const repoDir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
  const pkgDir = path.join(repoDir, 'node_modules', WIRE_PACKAGE);
  fs.mkdirSync(pkgDir, { recursive: true });
  fs.writeFileSync(
    path.join(pkgDir, 'package.json'),
    JSON.stringify({ name: WIRE_PACKAGE, version: '1.0.0', types: 'index.d.ts' })
  );
  fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), WIRE_PACKAGE_DTS);
  fs.writeFileSync(
    path.join(repoDir, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: {
        strict: true,
        module: 'esnext',
        moduleResolution: 'bundler',
        target: 'es2022',
        lib: ['es2022'],
        skipLibCheck: true,
      },
      include: ['src'],
    })
  );
  const pathOf = (name: string): string => path.join(repoDir, 'src', name);
  for (const [name, text] of Object.entries(sources)) {
    fs.writeFileSync(pathOf(name), text);
  }
  return { repoDir, pathOf };
}

/** A sidecar `infer` answer, as far as these tests read it. */
export interface WireInferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  primary_type_symbol?: string;
  array_depth?: number;
  raw_text_read?: boolean;
  any_provenance?: Array<{ path: string; kind: string; reason: string; detail?: string }>;
}

export const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();
