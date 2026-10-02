# Rust library API

Starting with Bioconda2RPM 0.2.0, the crate provides a supported Rust library
alongside its existing command-line application. The CLI remains the primary
user interface and keeps its existing behavior.

The initial library surface is intentionally narrow:

- `bioconda2rpm::recipe_repo` prepares a local Bioconda recipes checkout through
  `RecipeRepoRequest`, `ensure_recipe_repository`, and `RecipeRepoOutcome`.
- `bioconda2rpm::ingress` validates recipe names, checks for recipe directories,
  and reads normalized recipe metadata through `is_valid_recipe_name`,
  `recipe_exists`, `lookup_recipe_metadata`, and `RecipeMetadata`.

The ingress metadata reader applies the same selector and metadata rendering
logic as the CLI. Recipe release dates are inferred from filesystem metadata
when available; they are not authoritative upstream release timestamps.

Recipe names `.` and `..` are invalid. Recipe lookup resolves the recipe root,
recipe directory, selected version directory, and metadata file to canonical
paths and rejects any candidate that escapes the recipe root, including
case-insensitive directory matches and symlinks. `recipe_exists` returns an
error for an escaping directory; `lookup_recipe_metadata` returns an error for
invalid names or escaping paths. The `meta_yaml_path` in `RecipeMetadata` is the
contained canonical metadata path.

The API is available to Rust consumers through the `bioconda2rpm` crate. No
stability guarantee is made beyond the crate's SemVer contract; additions before
1.0.0 may introduce a MINOR version increment.
