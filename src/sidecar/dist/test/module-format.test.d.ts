/**
 * carrick#1619: the init'd project resolved every import as a `require`.
 *
 * Under `module` `node16`..`nodenext` the compiler picks an import's export
 * conditions from the importing file's own format: its nearest `package.json`
 * `"type"`, or its extension. ts-morph creates each file without one, so every
 * import in `infer`, `bundle` and `retype_check` resolved with the `require`
 * conditions, and a package whose `exports` entry only has an `import` branch
 * did not resolve at all. The capture uses the compiler directly and read the
 * same import fine, so the two disagreed on one service: `infer` printed the
 * unresolved name, and a surface anchor built from that text dangled.
 *
 * The fixture is the workspace shape that found it: a sibling package whose
 * types resolve to source only through a custom `import` condition. The
 * CommonJS consumer is the control: the compiler resolves its import with the
 * `require` conditions and finds nothing, so it must stay unresolved here too.
 */
export {};
