/**
 * v2 check core — corpus end-to-end over the JSON-over-stdio wire.
 *
 * Drives the sidecar exactly as the Rust client will (WP3): capture_v2 to build
 * stub packages for real corpus services, then check_v2 to verify matched
 * pairs, handling the async install protocol's `status: 'progress'` keepalive
 * frames. Uses one corpus-2 pair and one corpus-3 pair (acceptance: two corpus
 * fixtures), both deliberately-incompatible edges from the fixtures, and pins
 * that the full verdict payload is byte-stable across two independent runs.
 *
 * All four fixtures are bare checkouts with no external-dep references in the
 * captured closures, so the check install is local-only (network-free).
 */
export {};
