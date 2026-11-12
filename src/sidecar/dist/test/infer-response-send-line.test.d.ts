/**
 * carrick#2005: a file-based route's response row is located at a send's own
 * line, and a send's line is not a route registration. The rule that types a
 * route from every success send of the handler the located send sits in
 * (carrick#1161) must run there too, and must read a status an earlier call
 * of the send's chain states (`reply.status(401).json(body)`).
 *
 * Every name below is a synthetic stand-in. `reply-kit` carries the transport
 * surface a Node response object has; nothing consults its name.
 */
export {};
