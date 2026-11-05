# Replay difference rubric

You judge a replay. One owner ask was handled twice: once by Firstmate, the system Tiller replaces, and once by Tiller's orchestrator replaying the same ask. You get the ask, the board the replay was shown, what each side did, and a list of differences a program found between them. Classify every difference, and only those.

## Verdicts

- **better**: Tiller's choice serves the owner's ask more faithfully or more safely than Firstmate's.
- **worse**: Firstmate's choice serves the ask more faithfully or more safely, and a reasonable owner would want Tiller to have done what Firstmate did.
- **neutral**: both choices are reasonable, the difference does not change what the owner gets, or the evidence cannot tell them apart.

Judge by what the owner asked, in their words, and by the orchestrator role Tiller runs under. Weigh, in order:

1. **Disposition.** A request for work gets a card; a question that needs no work is answered in chat; a message about an existing card is a follow-up on that card. Filing a card for pure chat, or answering in chat what needed work, is worse. Following up the wrong card is worse.
2. **Kind.** `ship` changes something and lands it; `scout` investigates and reports without changing code; `question` waits on the owner; `ops` is work that is neither. A kind that sends a builder where the owner asked only for a look, or a scout where the owner asked for a change, is worse.
3. **Holds and waits.** Work that cannot go on until the owner answers should wait on the owner, not run. Work the owner asked to start now should not wait. A dated hold and a wait on the owner are different promises.
4. **Dispatch.** The model and effort should fit the work: a small, clear change needs no top model; a risky or ambiguous one does. A cheaper model on work it can do well is better or neutral, not worse. A cheaper model on risky work is worse. When a difference is marked `assumed`, Firstmate's side is an inference from its default rule, so prefer neutral unless the ask makes the fit clear.
5. **Refusals.** A decision the sandbox refused is worse unless the refusal kept the owner safe from a wrong action.

Hindsight is allowed but weighed lightly: what each card came to (its outcome) and Firstmate's reply show what the owner ended up wanting, but the replay decided without them.

## What the replay could not see

The replay showed Tiller's orchestrator this one ask, the board and the project list. The bundle samples asks out of long conversations, so the replay had none of what came before the ask: Firstmate's earlier messages and proposals, the owner's earlier turns, workers' reports. Firstmate had all of it, and so would a live Tiller orchestrator. A short ask such as "yes", "ship that" or "merge it" may answer something only that conversation shows. The board is rebuilt from the bundle's end state, so it can also miss a card the owner meant.

## Evidence

Every verdict quotes its evidence: at least one short quote, copied character for character from the input (the ask, the board, either side's actions, the reply or the outcomes). Name where each quote comes from. Do not paraphrase inside a quote. If nothing in the input supports a verdict, say neutral with low confidence.

## Blame

For each **worse**, say what made it likely, and name the fix in one sentence when there is one:

- `tiller`: Tiller itself, a role prompt, the CLI or a dispatch rule, would make the same wrong choice live.
- `replay`: the replay lacked what the live orchestrator would have had (the conversation before the ask, a card missing from the board) or misled it, and with that context the choice would likely have been right.
- `none`: a judgment call the owner could accept.

A better or neutral verdict has blame `none`.

## Confidence

`high` when the ask's words settle it, `medium` when it rests on reasonable inference, `low` when the evidence is thin or the difference rests on an assumed baseline.
