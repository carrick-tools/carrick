/**
 * carrick#2472 C2: the response-site walk asks the service's server-framework
 * labels which calls are body sends before it falls back to its structural
 * reading. A labelled `send` member is a send wherever it sits in the
 * handler; an unlabelled member of a reply object that has labelled sends is
 * a setter (a header, a cookie, a status), never a body. Members are matched
 * by declaration, never by name.
 *
 * The fixture (`fixtures/labelled-sends`) is an invented server library whose
 * reply object returns itself from sends and setters alike. Each row is
 * located at an error send, so the walk publishes the handler's success
 * sends instead (carrick#2427).
 */
export {};
