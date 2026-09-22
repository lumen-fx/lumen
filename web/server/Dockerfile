# The lumen-server container image, published as ghcr.io/lumen-fx/lumen-server
# by .github/workflows/server-image.yml.
#
# Nothing compiles here. The image copies the release binary the toolchain
# build already produced for the target architecture, so the server in the
# image is byte for byte the one in lumen-linux-<arch>.tar.gz. The build
# context holds one binary per architecture, named by Docker's TARGETARCH:
#
#   bin/amd64/lumen-server
#   bin/arm64/lumen-server
#
# To build it for this machine from a binary you have (arm64 in place of
# amd64 on an Arm machine):
#
#   mkdir -p ctx/bin/amd64
#   cp /path/to/lumen-server ctx/bin/amd64/
#   docker build -f web/server/Dockerfile -t lumen-server:local ctx
#
# The base carries glibc, libgcc_s and CA certificates and nothing else: no
# shell, no package manager. Its glibc has to be at least as new as the one on
# the runner that linked the binary (Ubuntu 24.04), which is why it is the
# Debian 13 image and not the Debian 12 one. The nonroot tag runs as uid 65532.
#
# The site goes at /site, built by `lumenc web --render ssr`. Mount it
# read-only:
#
#   docker run -p 8080:8080 -v "$PWD/dist:/site:ro" ghcr.io/lumen-fx/lumen-server
#
# or bake it into an image of your own:
#
#   FROM ghcr.io/lumen-fx/lumen-server
#   COPY dist /site
#
# /site is not declared a VOLUME, so the second form keeps what it copies.

FROM gcr.io/distroless/cc-debian13:nonroot

ARG TARGETARCH
ARG VERSION=dev
ARG REVISION=unknown

LABEL org.opencontainers.image.title="lumen-server" \
      org.opencontainers.image.description="Production server for Lumen sites rendered per request" \
      org.opencontainers.image.source="https://github.com/lumen-fx/lumen" \
      org.opencontainers.image.url="https://lumenfx.dev" \
      org.opencontainers.image.licenses="MPL-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"

COPY --chmod=0755 bin/${TARGETARCH}/lumen-server /usr/local/bin/lumen-server

# Inside a container the server has to listen on every interface to be
# reachable through a published port; the binary's own default is loopback.
ENV LUMEN_BIND=0.0.0.0 \
    LUMEN_PORT=8080 \
    LUMEN_SITE=/site

USER nonroot:nonroot
EXPOSE 8080

# `lumen-server probe` asks the server at LUMEN_BIND and LUMEN_PORT for its
# liveness path, so the check needs no curl in the image. It reads the same
# environment the server does: move the port with LUMEN_PORT rather than a
# flag, or the probe keeps asking the old one.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["lumen-server", "probe"]

# The server handles SIGTERM itself, draining before it exits, and reaps its
# own worker processes, so it runs as PID 1 with no init in front of it.
ENTRYPOINT ["lumen-server"]
