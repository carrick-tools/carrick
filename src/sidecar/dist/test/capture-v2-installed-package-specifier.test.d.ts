/**
 * An absolute specifier into an installed package becomes the package's bare
 * specifier in every stub file (carrick#1174).
 *
 * The v1 printer names a type that is not in scope by an absolute
 * `import("<checkout>/node_modules/.../index")`. Literal anchors carry that
 * text into the surface, and the specifier rewrite only knew how to map an
 * absolute path onto a file of the emitted tree. A path into an installed
 * package has no tree file, so it shipped as printed: the checkout root (or
 * the home directory, for a runtime's npm cache) inside a file the index
 * uploads, naming a module no other machine has.
 *
 * The rewrite now maps such a path to the package's public specifier plus
 * subpath (through its `exports` map when it has one, and the DefinitelyTyped
 * public name for `@types/*`), and pins the package it came from, so the check
 * workspace resolves the specifier against the pinned install.
 */
export {};
