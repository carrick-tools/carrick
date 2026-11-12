/**
 * A tree is a bare checkout when a dependency its package.json declares does
 * not resolve from the service root, not only when node_modules is absent
 * (carrick#2999). A pnpm workspace whose store is missing still has each
 * package's node_modules directory, full of links to nothing.
 */
export {};
