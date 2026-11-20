# wrapped-object-members

carrick#4083: an exported controller object whose members are callbacks
handed to a wrapper call (`list: wrap(async (req, res) => { … })`) states one
function per member, keyed `owner.member`, the same key an arrow-valued member
(`list: async (req, res) => { … }`) gets.

Before the fix each callback was named for the wrapper (`wrap_handler`), and
the extractor kept only the first, so a controller read as one function.

- `src/controllers/widgets.ts`: `export const widgetController = { … }`, three
  arrow members.
- `src/controllers/gadgets.ts`: the same table declared first and exported by
  `export default gadgetController`, with one member a function expression.
- `src/utils/wrap.ts`: the wrapper.
- `src/store.ts`: one helper per member, so each row's calls show it holds its
  own body.
- `src/routes.ts`: an Express router registering `controller.member` for each
  member, and one inline route callback that keeps its route-derived name
  (`get_health_handler`).

The model is replayed from `__llm__/`: it states the routes in `routes.ts`, with
the handler names a model gives them, and nothing else.
