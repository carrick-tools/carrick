/**
 * carrick#2111: the type the model named for a producer's response is checked
 * against the compiler's type of what the handler sends.
 *
 * The model names one bare symbol per row. Where it names the element of a
 * list the handler sends, a helper's input type where the helper's result is
 * sent, or a type the handler wraps before sending, the row published the
 * symbol and a consumer of the real body read incompatible.
 *
 * The `infer` action takes the model's symbols beside its requests
 * (`model_symbols`, joined by alias) and answers on each inferred type how the
 * symbol relates to the read (`model_symbol_relation`). The compiler decides, by assignability both ways. The
 * driver keeps, re-depths or drops the symbol on that answer.
 *
 * Every request here goes through the process over stdio, so a field the
 * request schema dropped would show as a missing answer.
 */
export {};
