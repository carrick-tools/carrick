# jsx-component-callers

carrick#1149: a JSX element is a call to the component it renders, so
`get_callers` on a component lists the functions that render it.

`src/components/PriceTag.tsx` is rendered from three files:

- `src/pages/ProductPage.tsx` imports it relatively and renders it
  self-closing.
- `src/pages/CartPage.tsx` is an arrow component that renders it twice. That
  is one edge, because an edge is one caller and one callee.
- `src/checkout/CheckoutSummary.tsx` imports it through the tsconfig `@/`
  alias.

`ProductPage` also renders `<Icons.Star />` through a namespace import, which
resolves like `Icons.Star()`. `Cart` in the same module is never rendered and
has no callers.

The negative control is `<nav>` in `ProductPage`, beside an exported function
named `nav`. The TypeScript compiler treats a lowercase tag as an intrinsic
element, so it names no binding and records no edge.

Driven by `tests/jsx_component_callers_test.rs` with an empty cassette
directory. Call edges are deterministic, so the fixture needs no model answers.
