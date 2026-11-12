/**
 * carrick#1836: an inference says which declaration each name it printed
 * bare means, when the request's own file cannot see that name.
 *
 * The structural printer hands some subtrees back to the compiler's print with
 * no enclosing declaration, and that print writes every named type by its bare
 * name. A database client's row type is a library's mapped type, so a field
 * typed by the client's enum (`status: EntityStatus`) prints bare. The file
 * the request names never imports that enum, so the name read there, or in the
 * capture's surface entry, means nothing (TS2304) and the member reads `any`.
 *
 * The compiler knew the symbol when it printed the name. The inference now
 * records it: `printed_names` lists, for each name its text prints that the
 * request's file does not resolve, the module that declares it and the export
 * path to it there. A name printed for two different declarations is listed
 * twice, so the reader can tell it is ambiguous.
 */
export {};
