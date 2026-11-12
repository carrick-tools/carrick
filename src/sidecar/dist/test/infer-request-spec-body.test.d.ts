/**
 * carrick#1841, request half: the body of a request call that states its URL
 * on its one object argument.
 *
 * A generated client issues every operation as
 * `client.post<…>({ url: '/items', ...options })`. The method's own parameter
 * types the body as `unknown`, and the call has one argument, so the readers
 * that follow a body or config parameter of the method's signature find
 * nothing. The body the operation sends is the `body` member of the object it
 * hands the method: written there, or carried in by a spread whose type says
 * what it is. The scanner locates such a row at the call.
 *
 * The fixture is a hand-written client of that shape with neutral names.
 */
export {};
