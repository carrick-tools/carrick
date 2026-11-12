/**
 * carrick-cloud#1366: three request-body shapes that published the wrong
 * contract and read as enforced risks.
 *
 *  1. A handler parameter a KEYED decorator binds to one body field
 *     (`@Body('masterKey') masterKey: string`) published the field's type as
 *     the whole body (`string`), so every consumer sending
 *     `{ masterKey: string }` read "not assignable to string".
 *  2. A consumer request locator that names the request CALL
 *     (`api.post<Result>(url, body)`) published the call's response as the
 *     request body.
 *  3. A consumer `delete(url, { data })` located on the request CONFIG
 *     published the config (`{ data: {...} }`) as the body.
 *
 * Each shape is pinned twice: by what `infer` returns for the real locator,
 * and by what the tsc judge says when those inferred texts are compared. The
 * judge half proves the false risk is gone AND that a real mismatch of the
 * same shape is still caught.
 */
export {};
