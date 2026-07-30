# podman build -t localhost/coaster-sandbox rust/coaster-bench/sandbox
# iproute2 is required for OpenShell's netns isolation; the sandbox user by
# the policy's process.run_as_user. python3 is for agents that want to compute
# track geometry before placing it; it gets no network, because the policy
# attributes network per binary and python3 is in no binaries list.
FROM node:22-bookworm-slim

# Pinned: a rebuild must reproduce the agent that produced past results.
ARG CLAUDE_VERSION=2.1.218

RUN apt-get update && apt-get install -y --no-install-recommends iproute2 ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g "@anthropic-ai/claude-code@${CLAUDE_VERSION}" \
    && useradd -m -s /bin/bash sandbox \
    && mkdir -p /workspace && chown sandbox:sandbox /workspace

WORKDIR /workspace
CMD ["sleep", "infinity"]
