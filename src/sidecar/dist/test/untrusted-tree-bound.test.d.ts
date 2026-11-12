/**
 * carrick#2230 part 2: under CARRICK_UNTRUSTED_TREE the compiler reads no file
 * whose real path is outside the checkout.
 *
 * A scan on a machine that scans repositories it does not own reads only the
 * repository it was given. The fixture is generated here: a directory beside
 * the checkout holds a file carrying a sentinel, and the checkout reaches it
 * three ways. A tracked symbolic link, a relative import that climbs out, and
 * a tsconfig `include` that names the outside directory by absolute path. It
 * also holds an installed package whose link resolves INSIDE the checkout,
 * which must keep reading.
 *
 * The known positive comes first: with the switch unset the sentinel is read
 * (the capture publishes it, the loader's project holds it). Without that, a
 * refusal and a collapse look alike.
 */
export {};
