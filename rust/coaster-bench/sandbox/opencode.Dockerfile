# podman build -t localhost/coaster-or -f rust/coaster-bench/sandbox/opencode.Dockerfile rust/coaster-bench/sandbox
ARG BASE=localhost/coaster-sandbox
FROM ${BASE}

ARG OPENCODE_VERSION=1.18.4

RUN npm install -g "opencode-ai@${OPENCODE_VERSION}" && opencode --version
