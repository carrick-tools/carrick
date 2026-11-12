/**
 * The verifier schema compiles on its own (carrick#2434, the seam pin for
 * carrick-cloud#2201).
 *
 * The companion cloud's library store vendors `src/validators.ts` byte for
 * byte, without `types.ts`, and type-checks it alone before its schema tests
 * run. Its only import that resolves there is `zod`; `./types.js` does not,
 * so anything in this file that leans on the message types (a compile-time
 * assertion over `SidecarRequest`, say) reads them as `any` and breaks the
 * cloud's lister pin. This compiles the file the way the cloud's gate does:
 * alone, strict, NodeNext, with that one unresolved import expected and
 * every other diagnostic a failure. Key parity between the schema and the
 * types lives in `request-keys-declared.test.ts`.
 */
import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> the sidecar root.
const SIDECAR = path.resolve(__dirname, '..', '..');
/** The import the vendored copy cannot resolve, by design. */
const UNVENDORED_TYPES = "Cannot find module './types.js'";
describe('validators.ts compiles alone, as the cloud vendors it', () => {
    it('draws no diagnostic beyond the unresolved types import', () => {
        // A directory of its own under the sidecar root: `zod` resolves from the
        // sidecar's install, `./types.js` from nowhere.
        const dir = fs.mkdtempSync(path.join(SIDECAR, '.vendored-validators-'));
        try {
            const file = path.join(dir, 'validators.ts');
            fs.copyFileSync(path.join(SIDECAR, 'src', 'validators.ts'), file);
            const program = ts.createProgram([file], {
                noEmit: true,
                strict: true,
                skipLibCheck: true,
                target: ts.ScriptTarget.ES2022,
                module: ts.ModuleKind.NodeNext,
                moduleResolution: ts.ModuleResolutionKind.NodeNext,
                types: ['node'],
            });
            const problems = ts
                .getPreEmitDiagnostics(program)
                .map(d => ts.flattenDiagnosticMessageText(d.messageText, '\n'))
                .filter(text => !text.startsWith(UNVENDORED_TYPES));
            assert.deepStrictEqual(problems, []);
        }
        finally {
            fs.rmSync(dir, { recursive: true, force: true });
        }
    });
});
