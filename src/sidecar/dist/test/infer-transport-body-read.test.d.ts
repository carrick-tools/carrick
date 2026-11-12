/**
 * carrick#2714, carrick#1841: a consumer call that answers a request
 * library's response object (a status, headers and a request config around
 * the body in `data`) published that object as its expected response, so
 * every producer body read as missing the transport's members.
 *
 * The consumer's expected response is what the code reads the body as:
 *
 *  - the type the source states at the read of the payload member: a cast
 *    (`res.data as Widget`), an annotation (`const w: Widget = res.data`), or
 *    the declared return type of the function that returns it unchanged;
 *  - the payload member's own type where the call states it
 *    (`http.get<Widget>(url)`), as before;
 *  - nothing, decided (`machinery_envelope` at the root), where neither
 *    states it: the body is untyped, or the response object is handed on
 *    whole and its body is read elsewhere. A call whose result type the
 *    compiler took from its return position (`Promise<void>`) still returns
 *    the response object, so it is decided the same way.
 *
 * And where the source casts the body it read to a placeholder union and
 * narrows it later (`json as { data: Widget[] }`), the narrowed statement is
 * the body, reported as stated, so a model symbol naming the element inside
 * it is not kept.
 */
export {};
