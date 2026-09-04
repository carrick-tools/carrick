# flat-routes-declared-method

Flat-route modules whose handler is built by a route-builder call that takes
the HTTP method as an option (carrick#665). The convention's default for a
write export is POST; a route that declares `method: "PUT"` serves PUT, and
before this fixture existed every such route was recorded as a POST that
nothing serves — with its real consumer left unmatched against it.

Also pins what must NOT be narrowed: a verb that is not a literal, a `method`
nested inside another option, and a call whose result is destructured into two
handlers at once.
