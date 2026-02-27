# Phoreus RPM Packaging & Distribution Charter

**Version:** 1.1
**Applies to:** All RPM artefacts built and distributed under the Phoreus bioinformatics platform
**Target Platform:** RHEL-compatible systems (AlmaLinux, Rocky Linux, RHEL)
**Packaging Model:** Multi-version co-installable RPMs with Lmod module integration

---

# 1. Purpose

This charter defines the authoritative standards governing:

* Construction of RPM packages under the **Phoreus** software distribution framework
* Multi-version coexistence strategy
* Lmod modulefile integration
* Reproducibility guarantees
* Artefact immutability and validation policy
* Repository distribution workflow

This document is normative and must be followed by all automation agents, maintainers, and CI/CD systems producing Phoreus artefacts.

---

# 2. Design Principles

Phoreus RPMs are engineered around the following constraints:

## 2.1 Deterministic Reproducibility

Each upstream software version must produce a deterministic binary artefact.

* One upstream version → one immutable RPM
* Artefacts must not be rebuilt with different toolchains under the same NEVRA
* Rebuilds require `Release` bump

## 2.2 Multi-Version Coexistence

Multiple upstream versions of the same tool must be installable simultaneously.

This is achieved by:

```
phoreus-<tool>-<upstream_version>
```

being the **RPM Name**.

Example:

```
phoreus-jellyfish-2.3.1
phoreus-jellyfish-2.4.0
```

These RPMs:

* Install into versioned prefixes
* Do not conflict with each other
* Never overwrite shared files

## 2.3 Versioned Prefix Isolation

All tool installations must be confined to:

```
/usr/local/phoreus/<tool>/<version>
```

No files may be installed outside this prefix except:

```
/usr/local/phoreus/modules/<tool>/<version>.lua
```

The only shared location permitted is the module directory.

## 2.4 Default Version Abstraction

A separate meta package:

```
phoreus-<tool>
```

defines the validated production default.

This package:

* Depends on one specific versioned payload RPM
* Owns the `default.lua` symlink
* May be updated without removing older validated versions

---

# 3. Filesystem Layout Standard

## 3.1 Installation Prefix

```
/usr/local/phoreus/<tool>/<version>/
```

Example:

```
/usr/local/phoreus/jellyfish/2.4.0/
```

## 3.2 Modulefile Directory

```
/usr/local/phoreus/modules/<tool>/<version>.lua
/usr/local/phoreus/modules/<tool>/default.lua
```

Only the meta package owns `default.lua`.

---

# 4. Package Classes

Phoreus defines three RPM classes:

| Class               | Naming Convention        | Co-installable | Owns default.lua |
| ------------------- | ------------------------ | -------------- | ---------------- |
| Core Infrastructure | phoreus                  | N/A            | No               |
| Versioned Payload   | phoreus-<tool>-<version> | Yes            | No               |
| Meta Default        | phoreus-<tool>           | One per tool   | Yes              |

---

# 5. Core Infrastructure Package (phoreus)

This package establishes:

* Lmod dependency
* MODULEPATH configuration
* RPM macro definitions

## 5.1 SPEC Template — phoreus Core

```spec
Name:           phoreus
Version:        1.0.0
Release:        1%{?dist}
Summary:        Phoreus RPM infrastructure and module integration layer
License:        MPL-2.0
URL:            https://www.mnemosyne.co.uk/
BuildArch:      noarch

Requires:       Lmod

%global phoreus_root      /usr/local/phoreus
%global phoreus_modroot   %{phoreus_root}/modules

%description
Phoreus provides standardised RPM infrastructure for bioinformatics
deployment using Lmod-based multi-version module management.

%install
rm -rf %{buildroot}

# Profile integration
mkdir -p %{buildroot}/etc/profile.d

cat > %{buildroot}/etc/profile.d/phoreus.sh <<'EOF'
_pr_mod="/usr/local/phoreus/modules"
if [ -d "${_pr_mod}" ]; then
  case ":${MODULEPATH:-}:" in
    *:"${_pr_mod}":*) ;;
    *) export MODULEPATH="${MODULEPATH:+${MODULEPATH}:}${_pr_mod}" ;;
  esac
fi
unset _pr_mod
EOF

# RPM macro definitions
mkdir -p %{buildroot}/etc/rpm

cat > %{buildroot}/etc/rpm/macros.phoreus <<'EOF'
%global phoreus_root      /usr/local/phoreus
%global phoreus_prefix    %{phoreus_root}/%{tool}/%{version}
%global phoreus_moddir    %{phoreus_root}/modules/%{tool}

%global phoreus_write_modulefile() \
mkdir -p %{buildroot}%{phoreus_moddir}; \
cat > %{buildroot}%{phoreus_moddir}/%{version}.lua <<EOM \
help([[ %{summary} ]]) \
whatis("Name: %{tool}") \
whatis("Version: %{version}") \
whatis("URL: %{url}") \
prepend_path("PATH", "%{phoreus_prefix}/bin") \
prepend_path("LD_LIBRARY_PATH", "%{phoreus_prefix}/lib") \
prepend_path("MANPATH", "%{phoreus_prefix}/share/man") \
EOM \
%{nil}
EOF

%files
%config(noreplace) /etc/profile.d/phoreus.sh
%config(noreplace) /etc/rpm/macros.phoreus
```

---

# 6. Versioned Payload Package

## Naming

```
phoreus-<tool>-<upstream_version>
```

## SPEC Template — Versioned Payload

```spec
%global tool jellyfish

Name:           phoreus-%{tool}-2.4.0
Version:        2.4.0
Release:        1%{?dist}
Summary:        Jellyfish 2.4.0 built for Phoreus

License:        GPL-3.0-or-later
URL:            https://github.com/gmarcais/Jellyfish
Source0:        %{url}/releases/download/v%{version}/%{tool}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  make
Requires:       phoreus

%description
Versioned Phoreus build of %{tool}.

%prep
%autosetup -n %{tool}-%{version}

%build
%configure --prefix=%{phoreus_prefix}
%make_build

%install
rm -rf %{buildroot}
%make_install

%phoreus_write_modulefile

%files
%{phoreus_prefix}/
%{phoreus_moddir}/%{version}.lua
```

---

# 7. Meta Default Package

## Naming

```
phoreus-<tool>
```

## SPEC Template — Default Pointer

```spec
%global tool jellyfish

Name:           phoreus-%{tool}
Version:        1
Release:        2%{?dist}
Summary:        Default validated jellyfish for Phoreus
BuildArch:      noarch

Requires:       phoreus
Requires:       phoreus-%{tool}-2.4.0

%global phoreus_moddir /usr/local/phoreus/modules/%{tool}

%description
Meta package tracking the validated production version of %{tool}.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{phoreus_moddir}
ln -sfn 2.4.0.lua %{buildroot}%{phoreus_moddir}/default.lua

%files
%{phoreus_moddir}/default.lua
```

---

# 8. Update Procedure

When validating a new upstream version:

1. Build `phoreus-<tool>-<new_version>`
2. Modify meta package:

   * Update `Requires:` to new version
   * Update symlink target
   * Increment `Release`
3. Publish both RPMs
4. Regenerate repository metadata

Users updating:

```
dnf update phoreus-<tool>
```

Will:

* Install the new validated version
* Update default modulefile
* Preserve all previously installed versions

---

# 9. Repository Policy

* All artefacts must be GPG signed
* Repository metadata must be regenerated after every publish
* Artefacts must never be replaced in-place
* Obsolete payload RPMs must not be removed unless explicitly deprecated

---

# 10. Explicit Non-Goals

Phoreus does not:

* Use installonlypkg hacks
* Allow multiple RPMs with identical Name
* Allow payload packages to own shared module symlinks
* Permit in-place prefix mutation

---

# 11. Automation Guarantees

All SPEC files must be:

* Declarative
* Macro-driven
* Free of hard-coded absolute paths (except root prefix)
* Deterministic under CI build

Codex agents generating new tool wrappers must:

* Emit one versioned payload SPEC
* Optionally emit/update one meta SPEC
* Never modify historical SPEC files except for changelog entries

---

# 12. Compliance

Any RPM not conforming to this charter:

* Must not be published
* Must not be installed in validated production environments
* Must not override another version’s prefix

---

**End of Charter**
**Phoreus Packaging Authority**

