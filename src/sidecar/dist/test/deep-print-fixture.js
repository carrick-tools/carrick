/**
 * Two invented packages for the lister's bounded print (carrick#3744).
 *
 * `fixture-deep-print` exports a function whose return type is a deferred
 * conditional with no alias: each level names the level below twice, once at
 * `T` and once at `T | number`, so printing it in full doubles at every level
 * and runs the compiler out of heap. `fixture-plain-print` declares only the
 * ordinary export, so the two listings of `plain` can be compared.
 */
import * as fs from 'node:fs';
import * as path from 'node:path';
export const DEEP_LEVELS = 26;
export function deepDeclarations() {
    const lines = ['export interface Leaf<T> { value: T }', 'export type Flat0<T, R = T extends string ? Leaf<T> : Leaf<number>> = R;'];
    for (let level = 1; level <= DEEP_LEVELS; level++) {
        lines.push(`export type Flat${level}<T, R = T extends string ? Flat${level - 1}<T> : Flat${level - 1}<T | number>> = R;`);
    }
    lines.push(`export declare function flat<T>(value: T): Flat${DEEP_LEVELS}<T>;`);
    lines.push('export declare function plain(name: string): Leaf<string>;');
    return `${lines.join('\n')}\n`;
}
export function plainDeclarations() {
    return 'export interface Leaf<T> { value: T }\nexport declare function plain(name: string): Leaf<string>;\n';
}
/** A probe project in `dir` with both packages installed. */
export function writeDeepPrintProject(dir) {
    const files = {
        'package.json': JSON.stringify({ name: 'probe', private: true, type: 'module' }),
        'tsconfig.json': JSON.stringify({
            compilerOptions: { target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext', strict: true, skipLibCheck: true, noEmit: true },
            include: ['src'],
        }),
        'src/index.ts': 'export {};\n',
        'node_modules/fixture-deep-print/package.json': JSON.stringify({ name: 'fixture-deep-print', version: '1.0.0', types: 'index.d.ts' }),
        'node_modules/fixture-deep-print/index.d.ts': deepDeclarations(),
        'node_modules/fixture-plain-print/package.json': JSON.stringify({ name: 'fixture-plain-print', version: '1.0.0', types: 'index.d.ts' }),
        'node_modules/fixture-plain-print/index.d.ts': plainDeclarations(),
    };
    for (const [rel, text] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
        fs.writeFileSync(path.join(dir, rel), text);
    }
}
