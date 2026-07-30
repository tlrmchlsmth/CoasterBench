# Benchmark findings

Behavioural observations from CoasterBench runs. Each entry is dated and cites
the run it came from, so a claim can be checked against the artifacts.

## Laguna-S-2.1 in the interactive driver lane (2026-07-30)

**Runs:** `20260730-laguna-interactive-{1,2,3}` (driver-mcp harness, vLLM
0.25.1 on the pirate B200 cluster, ride type 51, no-graphics, 3 rounds each).

One-shot design prompts were already known to be hopeless for Laguna (its
thinking never terminates; see evals/ci/README.md finding 3). The interactive
per-piece lane works where one-shot did not — but only after three driver-side
recoveries, each worth zero-to-something on its own:

| Run | Driver behaviour | Rounds scored | Best |
| --- | --- | --- | --- |
| 1 | plain loop, 16k completion cap | 0/3 | — |
| 2 | + nudges on unbanked stop / mid-think cutoff | 2/3 | 0.63 |
| 3 | + 32k cap (cutoffs gone; unparsed-call stalls exposed) | in progress | — |

The three stall modes, all observed in transcripts:

1. **Mid-think truncation looks like stopping.** A geometry-planning turn can
   burn a 16k completion cap while still thinking; the reply has no tool call
   and a naive loop scores the round zero. Detect `finish_reason: length` and
   nudge; 32k made these rare.
2. **Voluntary early stop with an open circuit.** Laguna quits, far under
   budget, without calling finish_and_test. The nudge ("stopping now scores
   zero; close the circuit") sent it from 11-turn giveups to 44-turn builds.
3. **Unparsed compact tool-call syntax.** It emits
   `<tool_call>undo_piece()<tool_call>undo_piece()...` as text; poolside_v1
   parses zero calls out of that, which is indistinguishable from a decision
   to stop unless the driver looks for literal `<tool_call>` in the text.

Consequence for scoring fairness: these stalls are harness/protocol artifacts,
not design ability. A lane that does not recover from them measures the
parser, not the model.

## kimi-k3 does not end a round on its own (2026-07-24)

**Run:** `20260724-kimi-k3-twister` (opencode / OpenRouter, ride type 51).

In every round, kimi-k3 ran the full 30-minute wall-clock budget and was stopped
by the `--session-timeout` rather than ending the session itself:

| Round | Wall time | finish_and_test calls | Score |
| --- | --- | --- | --- |
| 1 | ~30 min (timeout) | 3 | 6.08 |
| 2 | ~29 min (timeout) | 5 | 7.05 |
| 3 | ~30 min (timeout) | 5 | 5.32 |

The pattern each round is the same: test a working coaster, then demolish and
rebuild to try for a higher score, repeating until the timeout.

Claude Code models on the same task behave differently. In
`20260723-sub-twister-1` (fable-5, `--max-turns 120`, no wall-clock timeout),
sessions ended on their own at 46-89 turns, below the cap.

Whether this is the model or the opencode harness is not yet settled. The two
lanes differ in more than the model: Claude Code enforces `--max-turns` while
opencode has no turn cap, so kimi may be running to the wall-clock only because
nothing else bounds it. Separating the two needs another opencode model that
builds a real coaster, which we do not yet have.

The budget prompt did not change this. The round prompt states the wall-clock
budget ("you have about 30 minutes... bank a tested circuit early"), and this
run used that prompt. A one-shot prompt can state a deadline but cannot make the
model converge, since it has no clock and can always try another rebuild.

### Consequences for the harness

- **Cost.** A model that uses the full budget every round costs the full budget.
  kimi averaged about $7-8 per round here.
- **Best-result scoring matters for models like this.** kimi is stopped
  mid-rebuild every round, so scoring the final park state would record no
  coaster each time. Scoring the best tested result of the round (the server's
  `best_result`) records the real work: round 1 scored 6.08 for the same
  situation that scored zero before the change.
- **Stopping a model early requires driving the loop directly.** A mid-session
  reminder ("N minutes left, finalise now") needs per-turn injection, which the
  one-shot `opencode run` and `claude -p` invocations do not allow. That means
  running the agent loop the way driver.py does rather than delegating to the
  harness CLI. Whether it is worth doing is open, now that scores are protected.
