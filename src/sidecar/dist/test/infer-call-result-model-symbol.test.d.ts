/**
 * Blueprint rows N27 (i) and N30 (carrick#3334, carrick#3336): the compiler,
 * not text, decides what a consumer call's model symbol and stated body name.
 *
 * N27 (i): where a call's result is untyped (`client.post(url, event)` on an
 * untyped client), the model sometimes names the type of the value the call
 * SENDS (`AuditEvent`) as the type it receives. The compiler relates the
 * model's symbol to each argument of the call; a symbol that is an
 * argument's type, or its element, is answered `different`, and the driver
 * drops it so the inference defines the alias.
 *
 * N30: where the source states the body it reads (`as readonly User[]`), the
 * stated body carries how its root's declared type relates to the statement
 * (`root_relation`). The driver re-aims at the root on `same` or `list`,
 * whatever the printed text; it never compares the text.
 *
 * Every request goes through the process over stdio, so a field the schema
 * dropped would show as a missing answer.
 */
export {};
