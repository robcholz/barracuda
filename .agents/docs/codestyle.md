# Rust API Surface Conventions

This file records the repo's rules for choosing `getset` versus hand-written
getters, and `From`/`TryFrom` versus named constructors. Apply them to every
new or edited Rust type.

## Getters

Prefer `getset` derives for trivial field accessors: a method that does nothing
except return one field with the same type, meaning, and visibility.

| Return | Derive | Attribute |
| --- | --- | --- |
| `&T` | `Getters` | `#[getset(get = "pub")]` |
| copied `Copy` value | `CopyGetters` | `#[getset(get_copy = "pub")]` |
| `&mut T` | `MutGetters` | `#[getset(get_mut = "pub")]` |

Rules:

- Preserve the existing method name and visibility with the attribute.
- Put the API doc on the field so generated getters stay documented under
  `missing_docs`.
- Only convert methods that return the field verbatim.

Keep a hand-written getter when it is any of:

- a view whose type differs from the field (`Vec<T>` to `&[T]`,
  `Option<Value>` to `Option<&Value>`, `String` to `&str` via `as_str`);
- parsing, validation, lookup, computation, or error handling;
- a `const fn`;
- a trait impl (`AsRef`, `Borrow`).

## From / TryFrom

Always implement `From` on the target type; never implement `Into` directly.
The blanket impl `impl<T, U> Into<U> for T where U: From<T>` provides
`.into()` for free.

Decision flow for a single-value conversion:

1. Can it fail? Use `impl TryFrom`.
2. Does it return a borrowed view without consuming `self`? Use `as_xxx()` or
   `impl AsRef`.
3. Does it consume `self` and produce one unambiguous, lossless target type?
   Use `impl From`.
4. Otherwise (extracting one field, lossy/codec mapping, classifier with a
   default) use a named `into_xxx()` / `from_xxx()`.

Do not implement `From` when:

- the constructor guards a validation boundary; keep the validating
  constructor private or named;
- the conversion drops part of the source;
- the mapping is lossy or has a fallback branch;
- the source type is `pub(crate)`; a public `From` would leak it, so keep a
  `pub(crate)` named constructor.

For `TryFrom`, use the crate's natural error type. If that type is too broad
for the single failure this conversion can produce, introduce a tighter error
type or keep a named fallible constructor.

Once a `From` or `TryFrom` impl exists, use it at call sites (`.into()`,
`.try_into()`, `?`) instead of a parallel helper.

## Atomics and `Arc`

Firmware code uses `portable_atomic` atomics and `portable_atomic_util::Arc` /
`Weak`, never `core::sync::atomic`, `alloc::sync::Arc`, or their `std`
re-exports: some targets lack native atomics, and the portable types keep one
implementation on every Platform. The root `clippy.toml` disallows the
standard types and CI denies `clippy::disallowed_types`.

Host-only crates (the Linux and macOS Platforms, the CLI, workspace tools,
build-time and proc-macro crates, benchmarks) carry their own `clippy.toml`,
which replaces the root file and allows the standard types. A host-only module
inside a firmware crate, such as one behind a host feature, allows the lint
locally with a reason.

`portable_atomic_util::Arc` cannot coerce to `Arc<dyn Trait>` on stable Rust;
build trait objects with `Arc::from(Box::new(value) as Box<dyn Trait>)`.
