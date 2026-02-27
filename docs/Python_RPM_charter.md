# Phoreus Python Packaging Policy Charter

**Version:** 1.0
**Authority:** Phoreus Packaging Authority
**Applies to:** All Python-based software distributed under the Phoreus framework
**Scope:** RPM-packaged Python applications deployed within `/usr/local/phoreus`

---

# 1. Purpose

This document defines the mandatory policy for packaging Python software under the Phoreus ecosystem.

It establishes:

* How Python artefacts must be built
* How dependency graphs must be resolved
* How isolation must be enforced
* How reproducibility must be guaranteed
* How conflicts between incompatible dependency trees must be handled

This policy is normative. All Codex agents, CI systems, and human maintainers must adhere strictly to its provisions.

---

# 2. Architectural Premise

Python dependency graphs are not system libraries.

Unlike C/C++ shared libraries:

* Python packages frequently pin exact versions
* Minor version changes can introduce runtime breakage
* ABI compatibility is not stable across many numeric libraries
* Dependency solvers produce non-deterministic results unless fully locked

Therefore:

> Python dependencies SHALL be treated as application-level artefacts, not shared infrastructure.

---

# 3. Core Policy Decision

## 3.1 Every Python Application Is Hermetically Isolated

Each Python application RPM MUST:

* Contain its own complete virtual environment
* Install no files into system `site-packages`
* Declare no RPM-level dependencies on shared python-* modules (except the interpreter module)
* Avoid reliance on globally installed Python libraries

Python applications are distributed as:

```text
/usr/local/phoreus/<tool>/<version>/venv/
```

The RPM acts as a distribution envelope for a frozen Python runtime stack.

---

# 4. Rationale

This approach is mandatory because:

1. Multiple bioinformatics tools frequently require mutually incompatible versions of:

   * numpy
   * pandas
   * scipy
   * matplotlib
   * pydantic
   * and other widely used packages

2. Attempting to resolve these at RPM dependency level results in:

   * Unsatisfiable dependency graphs
   * Forced upgrades breaking validated tools
   * Non-reproducible environments

3. Phoreus is a validated scientific environment. Determinism supersedes disk efficiency.

Duplication is acceptable. Nondeterminism is not.

---

# 5. Interpreter Policy

Mnemosyne Biosciences provides two sanctioned Python interpreters under Phoreus modules:

Example:

```text
phoreus-python-3.10
phoreus-python-3.11
```

Python application RPMs MUST:

* Explicitly depend on one interpreter module
* Never assume system `/usr/bin/python`
* Load interpreter module before activating virtual environment

Example requirement:

```spec
Requires: phoreus-python-3.10
Requires: phoreus
```

---

# 6. Dependency Locking Requirements

## 6.1 Lockfiles Are Mandatory

Every Python tool MUST provide:

```text
requirements.in
requirements.lock
```

The lockfile MUST be generated via:

```bash
pip-compile --generate-hashes requirements.in
```

The build MUST install using:

```bash
pip install --require-hashes -r requirements.lock
```

This ensures:

* Hash verification
* Version pinning
* Immutable dependency trees
* Protection against upstream drift

---

# 7. Build-Time Virtual Environment Creation

## 7.1 Mandatory Build Procedure

During `%build`, the RPM MUST:

1. Create a virtual environment
2. Upgrade pip deterministically
3. Install locked dependencies
4. Install the application into that environment

The environment MUST reside inside the Phoreus prefix.

---

# 8. Example SPEC Template — Python Application

```spec
%global tool bio
%global pyver 3.10

Name:           phoreus-%{tool}-1.2.0
Version:        1.2.0
Release:        1%{?dist}
Summary:        Bioinformatics tool bio packaged for Phoreus

License:        MIT
BuildArch:      x86_64

Requires:       phoreus
Requires:       phoreus-python-%{pyver}

BuildRequires:  python%{pyver}
BuildRequires:  python%{pyver}-pip

%global phoreus_prefix /usr/local/phoreus/%{tool}/%{version}

%description
Hermetically packaged Python application stack for %{tool},
including fully frozen dependency graph.

%prep
%autosetup

%build
python%{pyver} -m venv venv
source venv/bin/activate
pip install --upgrade pip
pip install --require-hashes -r requirements.lock
pip install .

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{phoreus_prefix}
cp -a venv %{buildroot}%{phoreus_prefix}/

%files
%{phoreus_prefix}/
```

---

# 9. Modulefile Requirements

The modulefile MUST:

1. Load the correct interpreter module
2. Set VIRTUAL_ENV
3. Prepend the venv bin directory to PATH

Example:

```lua
load("phoreus-python-3.10")

local prefix = "/usr/local/phoreus/bio/1.2.0"
setenv("VIRTUAL_ENV", pathJoin(prefix, "venv"))
prepend_path("PATH", pathJoin(prefix, "venv/bin"))
```

The module MUST NOT:

* Modify system PYTHONPATH
* Inject shared site-packages
* Depend on global pip state

---

# 10. Prohibited Practices

The following are strictly forbidden:

* `Requires: python3-numpy`
* Installing into `/usr/lib/python3.x/site-packages`
* Using unpinned `pip install`
* Allowing pip to resolve dependencies during runtime
* Using conda, mamba, or external environment managers
* Sharing site-packages across Phoreus tools

---

# 11. Upgrade Semantics

When a new version of a Python tool is validated:

1. Build a new versioned RPM
2. Do not mutate prior environments
3. Update the meta default package if required
4. Preserve all historical environments intact

No Python virtual environment may be modified in-place.

---

# 12. Disk Utilisation Policy

Duplication of numpy, pandas, etc. across multiple tool environments is:

* Expected
* Acceptable
* Operationally safe
* Architecturally correct

Scientific reproducibility outweighs storage optimisation.

---

# 13. Automation Requirements

Codex agents generating Python RPMs MUST:

* Detect Python projects
* Require a lockfile
* Generate hermetic venv-based builds
* Refuse to generate shared-dependency RPMs
* Embed dependency hashes
* Fail builds if hash verification fails

---

# 14. Compliance

A Python RPM that:

* Shares dependency trees
* Omits lockfiles
* Relies on global Python modules
* Uses dynamic dependency resolution

is non-compliant and must not be published.

---

# 15. Strategic Outcome

This policy ensures:

* Coexistence of mutually incompatible dependency graphs
* Deterministic deployment
* Regulatory defensibility
* Clean integration with Phoreus modules
* Elimination of cross-tool dependency bleed

Under Phoreus, Python applications are immutable, isolated runtime stacks distributed via RPM.

---

**End of Python Packaging Charter**
**Phoreus Packaging Authority**


