# Healing tests

These are remend 1.4.0's test suites (https://github.com/vercel/streamdown,
`packages/remend/__tests__`, Copyright Vercel, Inc., Apache License 2.0; see
`../../markdown/NOTICE`), run against `../../markdown/heal.ts`. The only change to
the files is their imports (`bun:test`, and `heal` in place of `remend`), the
removals below, and two expectations: a mark left open inside the label of a link
still on its way is closed inside the label (`[**bold link**](…)`), where remend
left it open (see `healLink` in `heal.ts`).

Removed, because `heal` leaves those features out (see the header of `heal.ts`):

- whole suites: `katex`, `html-tags`, `setext-heading`, `single-tilde`,
  `custom-handlers`, and `code-block-utils`, `utils`, `coverage-gaps`,
  `trailing-patterns`, `underscore-bug` (they import remend's internal modules or
  React);
- `streaming-properties`, which needs `fast-check` and `mdast-util-from-markdown`;
  the renderer's own streaming test (`../markdown.test.ts`) checks the property
  that matters here, that nothing shown is taken back;
- the cases for options `heal` does not take: the `linkMode: "text-only"` suite
  in `links` and its 2 cases in `broken-markdown-variants`, the "disabled handlers
  via options" suite, `comparisonOperators: false`, the KaTeX cases (7), a stripped
  incomplete HTML tag, and non-string input.
