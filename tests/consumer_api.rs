use bioconda2rpm::ingress::{
    RecipeMetadata, is_valid_recipe_name, lookup_recipe_metadata, recipe_exists,
};
use bioconda2rpm::recipe_repo::{RecipeRepoOutcome, RecipeRepoRequest, ensure_recipe_repository};
use std::fs;
use tempfile::tempdir;

#[test]
fn recipe_ingress_exposes_validation_existence_and_metadata() {
    let temp = tempdir().expect("tempdir should create");
    let recipes_root = temp.path().join("recipes");
    let recipe_dir = recipes_root.join("blast");
    fs::create_dir_all(&recipe_dir).expect("recipe directory should create");
    fs::write(
        recipe_dir.join("meta.yaml"),
        r#"
package:
  name: blast
  version: "2.16.0"
about:
  home: "https://example.org/blast"
  license: "Public Domain"
  summary: "NCBI BLAST suite"
requirements:
  run:
    - python >=3.10
"#,
    )
    .expect("metadata should write");

    assert!(is_valid_recipe_name("blast"));
    assert!(!is_valid_recipe_name("Bad Name"));
    assert!(!is_valid_recipe_name("."));
    assert!(!is_valid_recipe_name(".."));
    fs::write(
        temp.path().join("meta.yaml"),
        "package:\n  name: escaped\n  version: \"1\"\n",
    )
    .expect("parent metadata should write");
    assert!(!recipe_exists(&recipes_root, "..").expect("invalid names should be rejected"));
    assert!(lookup_recipe_metadata(&recipes_root, "..").is_err());
    assert!(recipe_exists(&recipes_root, "blast").expect("existence should resolve"));
    let metadata: RecipeMetadata =
        lookup_recipe_metadata(&recipes_root, "blast").expect("metadata should resolve");
    assert_eq!(metadata.recipe_name, "blast");
    assert_eq!(metadata.latest_release.as_deref(), Some("2.16.0"));
    assert_eq!(
        metadata.canonical_url.as_deref(),
        Some("https://example.org/blast")
    );
}

#[cfg(unix)]
#[test]
fn recipe_ingress_rejects_direct_and_case_insensitive_directory_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir should create");
    let recipes_root = temp.path().join("recipes");
    let outside_recipe = temp.path().join("outside");
    fs::create_dir_all(&recipes_root).expect("recipe root should create");
    fs::create_dir_all(&outside_recipe).expect("outside recipe should create");
    fs::write(
        outside_recipe.join("meta.yaml"),
        "package:\n  name: outside\n  version: '1.0'\n",
    )
    .expect("outside metadata should write");

    symlink(&outside_recipe, recipes_root.join("direct"))
        .expect("direct recipe symlink should create");
    assert!(recipe_exists(&recipes_root, "direct").is_err());
    assert!(lookup_recipe_metadata(&recipes_root, "direct").is_err());

    symlink(&outside_recipe, recipes_root.join("CaSeD"))
        .expect("case-insensitive recipe symlink should create");
    assert!(recipe_exists(&recipes_root, "cased").is_err());
    assert!(lookup_recipe_metadata(&recipes_root, "cased").is_err());
}

#[cfg(unix)]
#[test]
fn recipe_ingress_rejects_metadata_and_version_directory_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir should create");
    let recipes_root = temp.path().join("recipes");
    let recipe_dir = recipes_root.join("blast");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&recipe_dir).expect("recipe directory should create");
    fs::create_dir_all(&outside).expect("outside directory should create");
    fs::write(
        outside.join("meta.yaml"),
        "package:\n  name: outside\n  version: '1.0'\n",
    )
    .expect("outside metadata should write");

    symlink(outside.join("meta.yaml"), recipe_dir.join("meta.yaml"))
        .expect("metadata symlink should create");
    assert!(lookup_recipe_metadata(&recipes_root, "blast").is_err());

    fs::remove_file(recipe_dir.join("meta.yaml")).expect("metadata symlink should remove");
    let outside_variant = outside.join("2.0");
    fs::create_dir_all(&outside_variant).expect("outside version directory should create");
    fs::write(
        outside_variant.join("meta.yaml"),
        "package:\n  name: blast\n  version: '2.0'\n",
    )
    .expect("outside variant metadata should write");
    symlink(&outside_variant, recipe_dir.join("2.0"))
        .expect("version directory symlink should create");
    assert!(lookup_recipe_metadata(&recipes_root, "blast").is_err());
}

#[test]
fn recipe_repository_api_types_and_existing_checkout_path_are_available() {
    let temp = tempdir().expect("tempdir should create");
    let recipe_repo_root = temp.path().join("recipe-repo");
    let recipe_root = recipe_repo_root.join("recipes");
    fs::create_dir_all(recipe_root.join("blast")).expect("recipe directory should create");

    let request = RecipeRepoRequest {
        recipe_root: recipe_root.clone(),
        recipe_repo_root: recipe_repo_root.clone(),
        recipe_ref: None,
        sync: false,
    };
    let outcome: RecipeRepoOutcome =
        ensure_recipe_repository(&request).expect("existing recipes should be accepted");

    assert_eq!(outcome.recipe_root, recipe_root);
    assert_eq!(outcome.recipe_repo_root, recipe_repo_root);
    assert!(!outcome.managed_git);
    assert!(!outcome.fetched);
}

#[cfg(unix)]
#[test]
fn recipe_ingress_rejects_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir should create");
    let recipes_root = temp.path().join("recipes");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&recipes_root).expect("recipes root should create");
    fs::create_dir_all(&outside).expect("outside directory should create");
    fs::write(
        outside.join("meta.yaml"),
        "package:\n  name: escaped\n  version: \"1\"\n",
    )
    .expect("outside metadata should write");

    symlink(&outside, recipes_root.join("escaped-recipe")).expect("recipe symlink should create");
    assert!(recipe_exists(&recipes_root, "escaped-recipe").is_err());
    assert!(lookup_recipe_metadata(&recipes_root, "escaped-recipe").is_err());

    let recipe_dir = recipes_root.join("metadata-link");
    fs::create_dir_all(&recipe_dir).expect("recipe directory should create");
    symlink(outside.join("meta.yaml"), recipe_dir.join("meta.yaml"))
        .expect("metadata symlink should create");
    assert!(lookup_recipe_metadata(&recipes_root, "metadata-link").is_err());
}
