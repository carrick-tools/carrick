/**
 * Post-emit specifier rewrite pass (design doc, Capture step 3 -- REQUIRED
 * component). Declaration emit ships tsconfig-`paths`-mapped specifiers
 * verbatim (`@app/models/item` dangles after relocation into the stub), and
 * node-builder-printed import types can carry absolute source paths. Both
 * are mapped onto tree-relative specifiers here, at capture time, in the
 * producer's own resolution context -- `paths` cannot be fixed at check time
 * (program-global, and namespaces collide across stubs).
 *
 * An absolute path into an installed package becomes that package's bare
 * specifier (installed-package.ts). Other specifiers that map outside the
 * emitted tree are left untouched: the per-alias self-check classifies them
 * as dangling internals with a recorded reason, which is the honest outcome.
 */
import * as fs from 'node:fs';
import * as path from 'node:path';
import { installedPackageSpecifier } from './installed-package.js';
import { isRelative, matchPathsPattern, rewriteSpecifiers, } from './specifiers.js';
export function parsePathsPatterns(options, configPath) {
    const paths = options.paths;
    if (!paths)
        return [];
    // Since TS 4.1 `paths` without `baseUrl` resolves relative to the config
    // file; the parser records that base internally as pathsBasePath.
    const base = path.resolve(options.baseUrl ??
        (options.pathsBasePath || path.dirname(configPath)));
    const patterns = [];
    for (const [pattern, targets] of Object.entries(paths)) {
        const starIdx = pattern.indexOf('*');
        patterns.push({
            pattern,
            prefix: starIdx === -1 ? pattern : pattern.slice(0, starIdx),
            suffix: starIdx === -1 ? undefined : pattern.slice(starIdx + 1),
            targets: targets.map((t) => path.resolve(base, t)),
        });
    }
    return patterns;
}
/** Map an absolute source path to its tree-relative emitted file, if any. */
function treeFileFor(absSource, entryDir, emitted, outside) {
    const noExt = absSource.replace(/\.(d\.ts|ts|tsx|mts|cts)$/, '');
    const rel = path.relative(entryDir, noExt).split(path.sep).join('/');
    if (rel.startsWith('..')) {
        const resolved = path.resolve(noExt);
        return outside.get(resolved) ?? outside.get(path.join(resolved, 'index'));
    }
    for (const candidate of [`${rel}.d.ts`, `${rel}/index.d.ts`]) {
        if (emitted.has(candidate))
            return candidate;
    }
    return undefined;
}
/** Tree-relative .d.ts path -> extensionless specifier from `fromFile`. */
function relativeSpecifier(fromFile, toFile) {
    const fromDir = path.posix.dirname(fromFile);
    let rel = path.posix.relative(fromDir, toFile.replace(/\.d\.ts$/, ''));
    if (!rel.startsWith('.'))
        rel = `./${rel}`;
    return rel;
}
/**
 * `import("<absolute>").Name` occurrences: the absolute specifier and the
 * first name read off it, so an installed-package rewrite can find the entry
 * that exports that name.
 */
const ABSOLUTE_IMPORT_TYPE = /(import\s*\(\s*)(["'])(\/[^"']+)\2(\s*\))(\s*\.\s*)([A-Za-z_$][\w$]*)/g;
/**
 * Rewrite paths-mapped, absolute-internal and absolute installed-package
 * specifiers in every emitted file (carrick#1174 for the last: a path into
 * `node_modules` or a runtime npm cache becomes the package's bare specifier).
 */
export function rewriteEmittedSpecifiers(args) {
    const patterns = parsePathsPatterns(args.options, args.configPath);
    const emitted = new Set(args.files);
    const pins = {};
    const installs = {};
    let total = 0;
    const installed = (spec, importedName) => {
        const mapped = installedPackageSpecifier(spec, importedName);
        if (!mapped)
            return undefined;
        installs[mapped.install.name] = mapped.install.root;
        if (mapped.pin)
            pins[mapped.pin.name] = mapped.pin.version;
        return { specifier: mapped.specifier, exportName: mapped.exportName };
    };
    for (const file of args.files) {
        const absFile = path.join(args.typesDir, file);
        const original = fs.readFileSync(absFile, 'utf8');
        let importTypeRewrites = 0;
        // Import types first, while the member name is still attached to its
        // specifier; an in-tree target is left for the general pass below.
        const text = original.replace(ABSOLUTE_IMPORT_TYPE, (whole, open, quote, spec, close, dot, name) => {
            if (treeFileFor(spec, args.entryDir, emitted, args.outside))
                return whole;
            const replacement = installed(spec, name);
            if (replacement === undefined)
                return whole;
            importTypeRewrites++;
            return `${open}${quote}${replacement.specifier}${quote}${close}${dot}${replacement.exportName ?? name}`;
        });
        const { text: rewritten, rewrites } = rewriteSpecifiers(text, (spec) => {
            // Absolute paths: an emitted tree file, else an installed package.
            if (spec.startsWith('/')) {
                const target = treeFileFor(spec, args.entryDir, emitted, args.outside);
                return target ? relativeSpecifier(file, target) : installed(spec)?.specifier;
            }
            if (isRelative(spec))
                return undefined;
            // tsconfig-paths patterns, first matching target that exists in-tree
            // (mirrors the resolver's declaration-order semantics).
            for (const pattern of patterns) {
                const star = matchPathsPattern(spec, pattern);
                if (star === undefined)
                    continue;
                for (const targetTemplate of pattern.targets) {
                    const absTarget = targetTemplate.replace('*', star);
                    const target = treeFileFor(absTarget, args.entryDir, emitted, args.outside);
                    if (target)
                        return relativeSpecifier(file, target);
                }
                // Matched a pattern but no in-tree target: leave it for the
                // self-check to classify (never guess).
                return undefined;
            }
            return undefined;
        });
        if (rewrites + importTypeRewrites > 0) {
            args.guard.writeFile(absFile, rewritten);
            total += rewrites + importTypeRewrites;
        }
    }
    return { rewrites: total, pins, installs };
}
