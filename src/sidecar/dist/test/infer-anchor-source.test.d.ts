/**
 * carrick#1819: an inference that names an anchor symbol also reports the
 * file that declares it, on every path that reports the symbol.
 *
 * `primary_type_symbol_source` was reported by a parameter read, an
 * expression read, and a response whose anchor was read off the annotation as
 * written. The paths an HTTP row takes (a function's return, a response
 * payload, a call's result, a payload recovered from a response send)
 * reported the symbol alone, so the scanner could name the type and not say
 * where to read it.
 *
 * Which declaration is the source is what the expression path already
 * answers, and every path now answers the same:
 *
 *  - a name imported through a barrel: the file that declares it, never the
 *    barrel that re-exports it;
 *  - a name two files declare: the file of the declaration this type
 *    resolves to;
 *  - a name an installed package declares: that package's declaration file,
 *    as an absolute path. What a reader is shown for such a home is the
 *    scanner's decision, not made here.
 *
 * No symbol, no source.
 *
 * carrick#2148: a shape declared as an object-literal alias
 * (`type Receipt = { … }`) or an instantiated generic alias (`Page<Invoice>`)
 * anchors at the alias's name and file, as an interface does. The compiler
 * gives such a type the anonymous `__type` as its own symbol; the alias is
 * where its name lives.
 */
export {};
