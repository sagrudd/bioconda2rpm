# Changelog

## 0.3.0 — pending qualification

- Add a library-only build without terminal UI dependencies by disabling the
  default `cli` feature; keep the public ingress and recipe repository APIs.
- Preserve the default CLI and terminal UI. The binary requires `cli`, and the
  library no longer compiles its unused private UI module.
- Coordinate the pending Phoreus consumer through Kanon PR803, allocation
  `INT07_BIOCONDA_LIBRARY_ONLY_PENDING_ALLOCATION.md`. Native and consuming graph
  qualification and security checks remain required before supported promotion.

## 0.2.0

- Add a supported Rust library surface for recipe repository preparation and
  recipe ingress metadata used by Phoreus.
- Reject `.` and `..` as recipe names and confine recipe directories, selected
  version directories, and metadata reads to the canonical recipe root, including
  symlinked paths.
- Preserve the existing CLI behavior and repository synchronization support.
