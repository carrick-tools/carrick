import { fileURLToPath } from 'node:url';

// The `~` alias exists only here, in bundler code. No config file the scanner
// reads declares it, so a call through it records no edge and is counted.
export default {
  resolve: {
    alias: {
      '~': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
};
