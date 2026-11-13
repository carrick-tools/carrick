import server from 'fixture-plugin-server';

const app = server();
const SIZES = ['small', 'large'] as const;

export function sizeRoutes() {
  for (const size of SIZES) {
    app.post(`/widgets/sizes/${size}`, async () => ({ size }));
  }
}
