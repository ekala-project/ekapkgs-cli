# Linting & Formatting

## Rust Lints (workspace-wide)

- `unsafe_code = "forbid"` — no unsafe code allowed anywhere
- Clippy runs with `-D warnings` in CI — all warnings are errors
- Key clippy allows: `too_many_arguments`, `module_name_repetitions`
- Key clippy warns: `cloned_instead_of_copied`, `str_to_string`, `needless_pass_by_value`, `manual_let_else`, `match_same_arms`, `unnecessary_wraps`, `implicit_clone`, `inefficient_to_string`

## Rustfmt

Configured in `.rustfmt.toml`:
- Style edition 2024, max comment width 100
- Import grouping: Std, External, Crate
- Formats doc comments, macro bodies, strings

## Clippy

Configured in `clippy.toml`:
- `too_many_arguments` threshold: 8
- `enum_variant_size` threshold: 400
- `literal_representation` threshold: 8 (allows long hash literals)
