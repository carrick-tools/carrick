/**
 * carrick#2814: the inferrer marks a route's request body, or a subscriber's
 * payload, whose handler works when nothing is sent (`absent_body_tolerated`).
 *
 * A server library hands a handler `undefined` where a request carried no
 * body. A handler that defaults the body, or reads it only through `?.` or
 * `?? {}`, survives that; one that destructures it or reads a member off it
 * throws, so it requires a body whatever its type says. A handler handed on
 * whole, or declared where no body of it is in reach, answers nothing and is
 * not marked.
 */
export {};
