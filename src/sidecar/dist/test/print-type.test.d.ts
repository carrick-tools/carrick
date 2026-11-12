/**
 * carrick#2019: a type from a module the enclosing file does not import
 * prints as `import("<declaring file>").Name`, whichever file it is printed
 * from, as it did on TypeScript 5.
 *
 * TypeScript 6 gives every print a module-specifier host, so the compiler's
 * own `typeToString` at an enclosing node writes `import("../deep/model")` or
 * `import("lib")`: text that depends on the file it was printed from, which a
 * capture pastes into another file and the scanner's path scrub cannot read.
 * `typeToStringAsDeclared` is `typeToString` without that host; on every type
 * whose print names no module it must give `typeToString`'s exact text.
 */
export {};
