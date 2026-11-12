/**
 * carrick#2057: a request located at a member of the body its handler parses.
 *
 * A handler reads its body off the platform request once and binds it
 * (`const body = (await request.json()) as ImportBody`). A locator that names
 * a member of that binding (`body.items`), or a name destructured out of the
 * read, names ONE field of the body. Publishing that field's type states the
 * whole body is a list of entries, and every caller that sends the object
 * reads incompatible.
 *
 * The body is what the source states at the read: a cast on it, or the
 * annotation of the binding it initialises. Where the source states nothing,
 * nothing is published. A locator that names the binding itself, or a member
 * of anything that is not the parsed body, keeps its reading.
 *
 * The platform types are TypeScript's own `dom` library; no framework or
 * method name is matched.
 */
export {};
