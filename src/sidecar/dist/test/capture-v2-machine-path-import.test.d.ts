/**
 * A stub never names a module by a machine path (carrick#2943).
 *
 * A literal anchor's text can name a type by `import("<absolute path>").Name`
 * where the printer that wrote it found no specifier: a workspace sibling's
 * SOURCE reached through a `node_modules` link, for one. The declaration emit
 * does not carry that file (the compiler treats it as a library), and it is
 * neither in the stub's tree nor an installed package, so the specifier
 * rewrite left the path as printed. On the machine that ran the capture the
 * path resolved to the source; after upload it became `<checkout>/...` and
 * resolved nowhere. The same pair then read one way judged from the fresh
 * stub and another from the stored one.
 *
 * A path the tree cannot hold is now `unknown` at its own position, labelled
 * as a reference that did not resolve, and the rest of the type survives. The
 * stub reads the same wherever the checkout sits.
 */
export {};
