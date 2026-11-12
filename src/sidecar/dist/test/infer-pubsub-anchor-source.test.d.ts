/**
 * carrick#413 (two-anchor arbitration): the pub/sub infer kinds
 * (`function_param` for subscribers, `expression` for publishers) must report
 * the deterministic anchor of the resolved payload — `primary_type_symbol`
 * plus its declaration file (`primary_type_symbol_source`) — so the scanner
 * can compare the tsc-resolved root against an LLM-emitted explicit symbol
 * and, on a witnessed borrow, re-aim the explicit bundle request at the real
 * payload type. The type STRING keeps ts-morph's default named form; the
 * anchor is what makes the demotion self-contained.
 *
 * Anonymous payloads (destructured wrapper generics) stay anchor-less: no
 * root symbol means the arbitration never fires for them.
 */
export {};
