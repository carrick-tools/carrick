/**
 * Wire contract for the v2 capture bundle ("tsc as the serializer").
 *
 * This file IS the seam (design doc: "seam, not split"): everything outside
 * src/sidecar/src/capture/ may import types from this file and the
 * `captureStub` entry point from ./index.js, and nothing else. Modules inside
 * capture/ import only node builtins, `typescript`, and each other. The
 * stdio `capture_v2` action is the only surface the Rust client sees.
 */
export {};
