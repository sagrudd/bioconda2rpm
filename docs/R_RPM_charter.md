# Phoreus R Packaging Policy Charter

**Version:** 1.0
**Authority:** Phoreus Packaging Authority
**Applies to:** All R-based software distributed under the Phoreus framework
**Scope:** RPM-packaged R applications deployed within `/usr/local/phoreus`

---

# 1. Purpose

This document defines the mandatory policy for packaging R software under the Phoreus ecosystem.

It establishes:

* How R artefacts must be built
* How CRAN and Bioconductor dependency graphs must be resolved
* How isolation must be enforced
* How reproducibility must be guaranteed
* How circular and mutually incompatible dependency chains must be handled

This policy is normative. All Codex agents, CI systems, and human maintainers must adhere strictly to its provisions.

---

# 2. Architectural Premise

R dependency graphs are not system libraries.

Unlike shared ELF libraries:

* R packages frequently rely on specific minor versions of R
* Bioconductor releases are tightly coupled to R minor versions
* CRAN packages evolve rapidly with implicit version expectations
* “Depends”, “Imports”, “LinkingTo”, and “Suggests” create deep and circular dependency graphs
* Live resolution against CRAN or Bioconductor is non-deterministic over time

Therefore:

> R package dependencies SHALL be treated as application-level artefacts, not shared infrastructure.

---

# 3. Core Policy Decision

## 3.1 Every R Application Is Hermetically Isolated

Each R-based RPM MUST:

* Contain its own complete R library tree
* Install no packages into system `site-library`
* Declare no RPM-level dependencies on shared R-* module packages (except the interpreter module)
* Avoid reliance on globally installed R libraries

R applications are distributed as:

```text
/usr/local/phoreus/<tool>/<version>/R/library/
```

The RPM acts as a distribution envelope for a frozen R runtime stack.

---

# 4. Rationale

This approach is mandatory because:

1. Bioinformatics tools frequently depend on tightly versioned ecosystems such as:

   * Bioconductor
   * tidyverse
   * GenomicRanges
   * Biostrings
   * DESeq2
   * edgeR

2. Attempting to model these as shared RPM dependencies results in:

   * Circular dependency chains
   * Bioconductor/R minor mismatches
   * Forced upgrades breaking validated tools
   * Inconsistent runtime behaviour across nodes

3. Phoreus is a validated scientific infrastructure. Determinism supersedes storage efficiency.

Duplication is acceptable. Nondeterminism is not.

---

# 5. Interpreter Policy

Mnemosyne Biosciences provides sanctioned R interpreters under Phoreus modules.

Example:

```text
phoreus-R-4.2
phoreus-R-4.3
```

R application RPMs MUST:

* Explicitly depend on one interpreter module
* Never assume `/usr/bin/R`
* Load the interpreter module before activating the local library tree

Example requirement:

```spec
Requires: phoreus-R-4.3
Requires: phoreus
```

---

# 6. Dependency Freezing Requirements

## 6.1 renv Lockfiles Are Mandatory

Every R-based tool MUST include:

```text
renv.lock
```

The lockfile MUST be generated via:

```r
renv::snapshot()
```

The build MUST restore dependencies using:

```r
renv::restore(lockfile = "renv.lock", prompt = FALSE)
```

## 6.2 Repository Snapshot Pinning

All builds MUST:

* Pin CRAN repository to a fixed snapshot date
* Pin Bioconductor release version explicitly
* Avoid live CRAN resolution during production builds

This ensures:

* Version determinism
* Stable binary resolution
* Protection against upstream package removal or mutation

---

# 7. Build-Time Library Tree Creation

## 7.1 Mandatory Build Procedure

During `%build`, the RPM MUST:

1. Create a local R library directory
2. Set `R_LIBS_USER` to that directory
3. Restore packages strictly from `renv.lock`
4. Install the application into that isolated tree

The library MUST reside inside the Phoreus prefix.

---

# 8. Example SPEC Template — R Application

```spec
%global tool myRtool
%global rver 4.3

Name:           phoreus-%{tool}-1.0.0
Version:        1.0.0
Release:        1%{?dist}
Summary:        R-based bioinformatics tool packaged for Phoreus

License:        GPL-3.0-or-later
BuildArch:      x86_64

Requires:       phoreus
Requires:       phoreus-R-%{rver}

BuildRequires:  R

%global phoreus_prefix /usr/local/phoreus/%{tool}/%{version}

%description
Hermetically packaged R application stack for %{tool},
including fully frozen CRAN/Bioconductor dependency graph.

%prep
%autosetup

%build
mkdir -p buildlib
export R_LIBS_USER=$(pwd)/buildlib

Rscript -e 'install.packages("renv", repos="https://cran.r-project.org")'
Rscript -e 'renv::restore(lockfile="renv.lock", prompt=FALSE)'

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{phoreus_prefix}/R
cp -a buildlib %{buildroot}%{phoreus_prefix}/R/library

%files
%{phoreus_prefix}/
```

---

# 9. Modulefile Requirements

The modulefile MUST:

1. Load the correct R interpreter module
2. Set `R_LIBS_USER` to the isolated library tree
3. Avoid modifying system R library paths

Example:

```lua
load("phoreus-R-4.3")

local prefix = "/usr/local/phoreus/myRtool/1.0.0"
setenv("R_LIBS_USER", pathJoin(prefix, "R/library"))
```

The module MUST NOT:

* Modify `/usr/lib64/R/library`
* Inject shared CRAN packages
* Depend on global user library state

---

# 10. Handling Circular and Suggests Dependencies

R dependency graphs frequently include:

* Circular “Suggests” relationships
* Vignette-only dependencies
* Test-time-only packages

Policy:

* Install only `Depends`, `Imports`, and `LinkingTo` by default
* Exclude `Suggests` unless required for runtime
* Disable vignette builds in production RPMs
* Avoid installing test frameworks in production artefacts

Example:

```r
install.packages("mypkg",
  dependencies = c("Depends","Imports","LinkingTo"))
```

---

# 11. Prohibited Practices

The following are strictly forbidden:

* Installing into `/usr/lib64/R/library`
* `Requires: R-dplyr`
* Live CRAN resolution during runtime
* Allowing package upgrades after RPM installation
* Sharing R library trees across tools
* Mixing Bioconductor releases across tools
* In-place mutation of library directories

---

# 12. Upgrade Semantics

When a new version of an R tool is validated:

1. Build a new versioned RPM
2. Do not modify previous library trees
3. Update the meta default package if required
4. Preserve historical versions intact

No R library tree may be mutated in-place.

---

# 13. Disk Utilisation Policy

Duplication of CRAN and Bioconductor packages across tool environments is:

* Expected
* Acceptable
* Operationally safe
* Architecturally correct

Scientific reproducibility outweighs storage optimisation.

---

# 14. Automation Requirements

Codex agents generating R RPMs MUST:

* Detect R-based projects
* Require a renv lockfile
* Refuse to build without repository snapshot pinning
* Enforce isolated library tree creation
* Fail builds if lockfile restoration fails
* Prevent shared-library packaging attempts

---

# 15. Strategic Outcome

This policy ensures:

* Coexistence of mutually incompatible R dependency graphs
* Deterministic deployment
* Bioconductor/R version integrity
* Regulatory defensibility
* Clean integration with the Phoreus module system
* Elimination of cross-tool dependency contamination

Under Phoreus, R applications are immutable, isolated runtime stacks distributed via RPM.

---

**End of R Packaging Charter**
**Phoreus Packaging Authority**

