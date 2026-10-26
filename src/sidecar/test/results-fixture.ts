/**
 * An invented client package whose members take the general shapes a call's
 * return can have (carrick#2817). None is copied from a real package.
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

export const RESULTS_DTS = `// An invented client package. Each member is one general shape a call's
// return can take; none is copied from a real package.
export interface Reply<T> {
  data: T;
  status: number;
  headers: Record<string, string>;
  config: { url: string };
}
export type Settled<D, E> = { data: D; error: undefined } | { data: undefined; error: E };
export interface Res {
  ok: boolean;
}
export interface Pending extends Promise<Res> {
  json<J = unknown>(): Promise<J>;
  text(): Promise<string>;
}
export interface Page<T> {
  items: T[];
  next: string | null;
}
export type LoadResult<D, E> =
  | { status: 'pending'; data: undefined; error: null; refetch(): void }
  | { status: 'error'; data: undefined; error: E; refetch(): void }
  | { status: 'success'; data: D; error: null; refetch(): void };
export interface SaveResult<D, E, V> {
  data: D | undefined;
  error: E | null;
  variables: V | undefined;
  mutate(v: V): void;
  mutateAsync(v: V): Promise<D>;
}
export declare const api: {
  /** 1: a reply wrapper, its payload type a defaulted parameter. */
  get<T = any, R = Reply<T>>(url: string): Promise<R>;
  /** 2: a data/error union, chosen by a flag. */
  request<TData, TError, Throw extends boolean = false>(options: {
    url: string;
    throwOnError?: Throw;
  }): Throw extends true
    ? Promise<{ data: TData; response: Res }>
    : Promise<Settled<TData, TError> & { response: Res }>;
  /** 3: the caller supplies the whole return. */
  send<T = unknown>(document: string, variables?: object): Promise<T>;
  /** 4: a pending object with a parse method. */
  fetch(url: string): Pending;
  /** 5: a paged wrapper. */
  list<T>(query: string): Promise<Page<T>>;
  /** 8: a plain object. */
  ping(): Promise<{ ok: boolean }>;
  /** 9: nothing returned. */
  close(): void;
  /** Overloads: only the first one wraps a payload. */
  multi<T>(key: string): Promise<Reply<T>>;
  multi(key: number): Promise<number>;
};
/** 6: a hook returning a state union. */
export declare function useLoad<TFnData = unknown, TError = Error, TData = TFnData>(options: {
  key: string;
  load: () => Promise<TFnData>;
}): LoadResult<TData, TError>;
/** 7: a mutation hook. */
export declare function useSave<TData, TError, TVars>(options: {
  save: (v: TVars) => Promise<TData>;
}): SaveResult<TData, TError, TVars>;
`;

/** A service root whose node_modules holds `fixture-results`. */
export function resultsService(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-results-'));
  const files: Record<string, string> = {
    'tsconfig.json': JSON.stringify({
      compilerOptions: {
        target: 'es2020',
        module: 'commonjs',
        moduleResolution: 'node',
        strict: true,
        esModuleInterop: true,
        skipLibCheck: true,
        types: [],
      },
      include: ['src/**/*.ts'],
    }),
    'package.json': JSON.stringify({ name: 'service' }),
    'src/index.ts': 'export const service = 1;\n',
    'node_modules/fixture-results/package.json': JSON.stringify({
      name: 'fixture-results',
      version: '1.0.0',
      types: 'index.d.ts',
    }),
    'node_modules/fixture-results/index.d.ts': RESULTS_DTS,
  };
  for (const [rel, text] of Object.entries(files)) {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
  return root;
}
