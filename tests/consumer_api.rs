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
