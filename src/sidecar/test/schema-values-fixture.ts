/**
 * Two invented packages for the foreign-maker gate (carrick#3745).
 *
 * `fixture-schema-kit` declares a generic builder interface whose every
 * method returns the builder again at the same type argument, plus a chain of
 * wrapper interfaces. `fixture-schema-values` exports many values typed by
 * that builder, each at its own type argument: the checker keeps one copy of
 * the builder's whole surface per value, and listing every member's receiver
 * on all of them runs the lister out of heap. It also exports a hub typed by
 * the kit (a foreign value with one member maker), a client typed by its own
 * package, a function, and the kit's own default builder, re-exported.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';

/**
 * The estimate the gate computes: per value, `wrappers + 3` member makers
 * (parse, describe, check and one per wrapper) times `wrappers + 4` members
 * (those and `output`), plus 1 for the hub (one maker, one member).
 */
export function foreignMakerEstimate(values: number, wrappers: number): number {
  return values * (wrappers + 3) * (wrappers + 4) + 1;
}

/** Member makers a gated listing leaves out: the values' own, plus the hub's. */
export function foreignMakersLeftOut(values: number, wrappers: number): number {
  return values * (wrappers + 3) + 1;
}

export function kitDeclarations(wrappers: number): string {
  const lines = [
    'export interface Base<T> { readonly output: T; parse(input: unknown): T; describe(text: string): this; }',
  ];
  const wraps = Array.from({ length: wrappers }, (_, k) => `w${k}(label: string): Wrap${k}<T>;`).join(' ');
  lines.push(`export interface Kit<T> extends Base<T> { check(test: (value: T) => boolean): Kit<T>; ${wraps} }`);
  for (let k = 0; k < wrappers; k++) {
    const steps = Array.from(
      { length: wrappers },
      (_, j) => `x${j}(label: string, test: (value: T) => boolean): Wrap${j}<T>;`
    ).join(' ');
    lines.push(`export interface Wrap${k}<T> extends Base<T> { ${steps} }`);
  }
  lines.push('export interface Room { send(event: string, payload: unknown): void; leave(): void; }');
  lines.push('export interface Hub { join(room: string): Room; }');
  lines.push('export declare const defaultKit: Kit<string>;');
  return `${lines.join('\n')}\n`;
}

export function valuesDeclarations(values: number): string {
  const lines = [
    "import type { Kit, Hub } from 'fixture-schema-kit';",
    'export declare const hub: Hub;',
    "export { defaultKit } from 'fixture-schema-kit';",
  ];
  for (let i = 0; i < values; i++) {
    lines.push(`export declare const Value${i}: Kit<{ id${i}: string; count${i}: number }>;`);
  }
  lines.push('export interface Channel { send(event: string, payload: unknown): void; close(): void; }');
  lines.push('export interface Client { request(method: string, params: unknown): Promise<unknown>; channel(name: string): Channel; }');
  lines.push('export declare const client: Client;');
  lines.push('export declare function build(): Kit<number>;');
  return `${lines.join('\n')}\n`;
}

/** A probe project in `dir` with both packages installed. */
export function writeSchemaValuesProject(dir: string, values: number, wrappers: number): void {
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'probe', private: true, type: 'module' }),
    'tsconfig.json': JSON.stringify({
      compilerOptions: { target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext', strict: true, skipLibCheck: true, noEmit: true },
      include: ['src'],
    }),
    'src/index.ts': 'export {};\n',
    'node_modules/fixture-schema-kit/package.json': JSON.stringify({ name: 'fixture-schema-kit', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/fixture-schema-kit/index.d.ts': kitDeclarations(wrappers),
    'node_modules/fixture-schema-values/package.json': JSON.stringify({
      name: 'fixture-schema-values',
      version: '1.0.0',
      types: 'index.d.ts',
      dependencies: { 'fixture-schema-kit': '1.0.0' },
    }),
    'node_modules/fixture-schema-values/index.d.ts': valuesDeclarations(values),
  };
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  }
}
