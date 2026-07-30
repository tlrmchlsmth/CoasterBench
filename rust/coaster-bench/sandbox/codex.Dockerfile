# podman build -t localhost/codex-arena -f rust/coaster-bench/sandbox/codex.Dockerfile rust/coaster-bench/sandbox
#
# The OpenShell base image ships codex 0.117 at /usr/bin/codex, too old for the
# gpt-5.6 model ids. The live codex-arena had a newer build dropped into
# /home/sandbox/bin/codex by hand and no record of where it came from, so this
# pins the version instead.
#
# The copy is deliberate: policy attribution uses kernel-resolved paths, so a
# symlink would be attributed to its target under node_modules, not to
# /home/sandbox/bin/codex as the policy expects.
FROM ghcr.io/nvidia/openshell-community/sandboxes/base:latest

ARG CODEX_VERSION=0.145.0-alpha.18

USER root

RUN set -eux; \
    npm install -g "@openai/codex@${CODEX_VERSION}"; \
    pkg="$(npm root -g)/@openai/codex"; \
    native="$(find "$pkg" -type f -path '*/vendor/*/bin/codex' | head -1)"; \
    test -n "$native"; \
    mkdir -p /home/sandbox/bin; \
    cp -L "$native" /home/sandbox/bin/codex; \
    chmod 0755 /home/sandbox/bin/codex; \
    chown -R sandbox:sandbox /home/sandbox/bin; \
    /home/sandbox/bin/codex --version

USER sandbox
