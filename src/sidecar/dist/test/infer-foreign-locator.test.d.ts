/**
 * carrick#2824: a located response expression that sits in another route's
 * handler is not the row's. The row's registration sits on its own line; a
 * locator pointing into the next registration's handler used to publish that
 * route's reply. The reader drops such a locator and answers by the line.
 *
 * A locator inside the row's own registration or handler, and one outside any
 * registration, is read as before.
 */
export {};
