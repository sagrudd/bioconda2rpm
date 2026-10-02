# Changelog

## 0.2.0

- Add a supported Rust library surface for recipe repository preparation and
  recipe ingress metadata used by Phoreus.
- Reject `.` and `..` as recipe names and confine recipe directories, selected
  version directories, and metadata reads to the canonical recipe root, including
  symlinked paths.
- Preserve the existing CLI behavior and repository synchronization support.
