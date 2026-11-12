/**
 * Partial-emit recovery reads an alias's specifiers the way the tree
 * placement and the specifier rewrite do (carrick#1773).
 *
 * When one file's declaration emit is skipped, the capture keeps the emitted
 * subset and demotes the aliases that name a module the tree lacks. That test
 * joined every specifier onto the surface's directory inside the tree, which
 * is only right for a relative specifier that stays under rootDir. It read
 * these as missing, and demoted the alias with "declaration emit was skipped
 * for module ...", although nothing they name was skipped:
 *
 *  - an absolute `import("/checkout/src/m")` path to a module under rootDir,
 *    which the specifier rewrite maps onto the tree file;
 *  - a module outside rootDir, by an absolute path or by `../`, whose
 *    declaration the placement puts under `__outside__/`;
 *  - an absolute path into an installed package, which the rewrite turns into
 *    the package's bare specifier and a pin. No emit writes that module.
 *
 * So on a service with one unnameable export, every literal anchor whose text
 * the inferrer printed with a path, and every literal that names a package's
 * type, read `unknown`.
 *
 * An alias that names the skipped module is still demoted, by whichever kind
 * of specifier names it, and so is one whose specifier reaches neither a tree
 * module nor an installed package. A RELATIVE path into a package stays
 * demoted too: no rewrite maps it, so kept it would ship a path out of the
 * stub (carrick#1857).
 */
export {};
