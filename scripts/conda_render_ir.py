#!/usr/bin/env python3
"""Render Bioconda recipe metadata via conda-build and emit normalized JSON."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
from typing import Any


def normalize_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, list):
        return [str(item).strip() for item in value if str(item).strip()]
    text = str(value).strip()
    return [text] if text else []


def first_url(source: Any) -> str:
    if isinstance(source, dict):
        url = source.get("url")
        if isinstance(url, list):
            return next((str(item).strip() for item in url if str(item).strip()), "")
        return str(url).strip() if url is not None else ""
    if isinstance(source, list):
        for item in source:
            url = first_url(item)
            if url:
                return url
    if isinstance(source, str):
        return source.strip()
    return ""


def source_folder(source: Any) -> str:
    if isinstance(source, dict):
        folder = source.get("folder")
        return str(folder).strip() if folder is not None else ""
    if isinstance(source, list):
        for item in source:
            folder = source_folder(item)
            if folder:
                return folder
    return ""


def source_patches(source: Any) -> list[str]:
    patches: list[str] = []
    if isinstance(source, dict):
        patches.extend(normalize_list(source.get("patches")))
    elif isinstance(source, list):
        for item in source:
            patches.extend(source_patches(item))
    return patches


def build_script(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, list):
        lines = [str(item).strip() for item in value if str(item).strip()]
        return "\n".join(lines) if lines else None
    text = str(value).strip()
    return text or None


def default_payload(recipe_dir: pathlib.Path, skip: bool) -> dict[str, Any]:
    return {
        "build_skip": skip,
        "package_name": recipe_dir.name,
        "version": "",
        "build_number": "0",
        "source_url": "",
        "source_folder": "",
        "homepage": "",
        "license": "NOASSERTION",
        "summary": f"Generated package for {recipe_dir.name}",
        "source_patches": [],
        "build_script": None,
        "noarch_python": False,
        "build_dep_specs_raw": [],
        "host_dep_specs_raw": [],
        "run_dep_specs_raw": [],
    }


def emit(payload: dict[str, Any]) -> int:
    json.dump(payload, sys.stdout, sort_keys=True)
    sys.stdout.write("\n")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Render recipe metadata via conda-build")
    parser.add_argument("recipe_dir", help="Recipe directory containing meta.yaml")
    args = parser.parse_args()

    recipe_dir = pathlib.Path(args.recipe_dir).resolve()
    payload = default_payload(recipe_dir, skip=False)

    try:
        from conda_build.api import render
    except Exception as exc:  # pragma: no cover - runtime environment dependent
        print(f"conda-build import failed: {exc}", file=sys.stderr)
        return 2

    try:
        rendered = render(str(recipe_dir), finalize=False, bypass_env_check=True)
    except TypeError:
        rendered = render(str(recipe_dir), finalize=False)
    except Exception as exc:  # pragma: no cover - runtime environment dependent
        print(f"conda-build render failed: {exc}", file=sys.stderr)
        return 3

    if not rendered:
        payload["build_skip"] = True
        return emit(payload)

    meta = rendered[0][0]
    payload["build_skip"] = False
    payload["package_name"] = str(meta.name()).strip() or recipe_dir.name
    payload["version"] = str(meta.version()).strip()

    build_number = meta.get_value("build/number", default=0)
    payload["build_number"] = str(build_number).strip() or "0"

    source = meta.get_value("source", default={})
    payload["source_url"] = first_url(source)
    payload["source_folder"] = source_folder(source)
    payload["source_patches"] = source_patches(source)

    about = meta.get_value("about", default={}) or {}
    payload["homepage"] = str(about.get("home") or "").strip()
    payload["license"] = str(about.get("license") or "NOASSERTION").strip() or "NOASSERTION"
    payload["summary"] = (
        str(about.get("summary") or "").strip()
        or f"Generated package for {payload['package_name']}"
    )

    payload["build_script"] = build_script(meta.get_value("build/script", default=None))

    noarch = meta.get_value("build/noarch", default=False)
    payload["noarch_python"] = str(noarch).strip().lower() == "python"

    payload["build_dep_specs_raw"] = normalize_list(
        meta.get_value("requirements/build", default=[])
    )
    payload["host_dep_specs_raw"] = normalize_list(
        meta.get_value("requirements/host", default=[])
    )
    payload["run_dep_specs_raw"] = normalize_list(
        meta.get_value("requirements/run", default=[])
    )

    return emit(payload)


if __name__ == "__main__":
    raise SystemExit(main())
