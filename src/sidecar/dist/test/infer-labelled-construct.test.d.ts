/**
 * carrick#3053 (H9): a handler that returns a labelled reply construction
 * (`new Reply(body)`, `Reply.json(body)`, a subclass's inherited `json`)
 * types its reply from the service's labels. A `new` send is keyed by its
 * construct signature's declaration, a static call by the member's
 * declaration; both are matched by declaration, never by name. Nothing here
 * names a platform class: `fixture-reply` is an invented package whose class
 * is not response-shaped, so only a label can say its construction is a send.
 */
export {};
