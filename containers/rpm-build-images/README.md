# RPM Build Container Images

This folder contains reference build images for `bioconda2rpm` SRPM/RPM execution.

Images provided:

- `Dockerfile.almalinux-10.1`
- `Dockerfile.almalinux-9.7`
- `Dockerfile.fedora-43`

Each Dockerfile is suitable for both `linux/amd64` and `linux/arm64` via `docker buildx`.

## Build (single architecture, local)

AlmaLinux 10.1:

```bash
docker buildx build \
  --platform linux/arm64 \
  --load \
  -t phoreus/bioconda2rpm-build:almalinux-10.1 \
  -f containers/rpm-build-images/Dockerfile.almalinux-10.1 \
  .
```

AlmaLinux 9.7:

```bash
docker buildx build \
  --platform linux/arm64 \
  --load \
  -t phoreus/bioconda2rpm-build:almalinux-9.7 \
  -f containers/rpm-build-images/Dockerfile.almalinux-9.7 \
  .
```

Fedora 43:

```bash
docker buildx build \
  --platform linux/arm64 \
  --load \
  -t phoreus/bioconda2rpm-build:fedora-43 \
  -f containers/rpm-build-images/Dockerfile.fedora-43 \
  .
```

Use `--platform linux/amd64` on x86_64 hosts for local testing.

## Build (multi-arch manifest)

AlmaLinux 10.1:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --push \
  -t phoreus/bioconda2rpm-build:almalinux-10.1 \
  -f containers/rpm-build-images/Dockerfile.almalinux-10.1 \
  .
```

AlmaLinux 9.7:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --push \
  -t phoreus/bioconda2rpm-build:almalinux-9.7 \
  -f containers/rpm-build-images/Dockerfile.almalinux-9.7 \
  .
```

Fedora 43:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --push \
  -t phoreus/bioconda2rpm-build:fedora-43 \
  -f containers/rpm-build-images/Dockerfile.fedora-43 \
  .
```

## Use with bioconda2rpm

```bash
cargo run -- build samtools \
  --recipe-root ../bioconda-recipes/recipes \
  --container-image phoreus/bioconda2rpm-build:almalinux-10.1
```
