/**
 * carrick#1841: a symbol the model names for a consumer's response that is a
 * response table keyed by status code (`GetItemResponses = { 200: Item }`,
 * the call's first type argument in a generated client) publishes the
 * table's 2xx body, not the table.
 *
 * An explicit symbol outranks inference in both places that resolve it: the
 * v1 bundle (which the index's type tools read) and the capture's symbol
 * anchor (which the check reads). Both read the rule, so both answers agree.
 * The request says the symbol names a consumer response; nothing reads the
 * alias spelling or the symbol's name.
 */
export {};
