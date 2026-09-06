// A pathless index segment (`_index`) contributes no path: this module serves
// `/admin` (carrick#702). It exports a read handler AND a component, so it is
// a route module that also renders a view — both facts, recorded together.

type AdminSummary = { orgs: number };

export const loader = makeAdminLoader(async (): Promise<AdminSummary> => {
  return { orgs: 3 };
});

export default function AdminIndexPage() {
  return null;
}

declare function makeAdminLoader<T>(handler: () => Promise<T>): unknown;
