#!/usr/bin/env python3
"""Run a command in its own session, so signals aimed at the parent's process
group cannot reach it, and print its pid.

Long benchmark runs need this: a shell or agent harness that reaps its
background jobs sends the signal to the whole group, which otherwise kills the
orchestrator, its game server and the agent session together.

    scripts/detach.py run.log ./rust/coaster-bench/target/release/coaster-bench \\
        --models claude-opus-5 --rounds 6 --fresh-sandbox --name my-run

Stop it deliberately with `kill -TERM <pid>`; coaster-bench handles that and
cleans up its server, agent and ephemeral sandbox.
"""

import os
import sys

if len(sys.argv) < 3:
    sys.exit(__doc__)

log, cmd = sys.argv[1], sys.argv[2:]

# Double fork: after the first the child is no longer a process group leader,
# which setsid() requires.
if os.fork() > 0:
    os.wait()
    sys.exit(0)
os.setsid()
pid = os.fork()
if pid > 0:
    print(pid, flush=True)
    os._exit(0)

fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
os.dup2(fd, 1)
os.dup2(fd, 2)
devnull = os.open(os.devnull, os.O_RDONLY)
os.dup2(devnull, 0)
os.execvp(cmd[0], cmd)
