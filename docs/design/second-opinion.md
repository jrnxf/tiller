Second opinion on `docs/design/plan.md`

I support an owned board, per-project orchestration, and an evidence-based improvement loop. I would make replacing Firstmate a later decision, and strengthen the state, recovery, approval, and measurement contracts before committing to that replacement.

This is a design review of the supplied plan. I checked the Claude Code documentation behind several important runtime assumptions and the cited AGENTS.md study. I have not independently reproduced the Firstmate measurements, audited its source, or verified every repository statistic and prior-art claim. Each item below separates my opinion from a suggested fix.

1. **Agree fully: migrate incrementally and preserve a working fallback.** Sections 1 and 13 have the right overall shape. Keeping useful work flowing while replacing one capability at a time is the strongest part of the proposal. Ownership is a legitimate objective even if the current system's output is good.

   **Preserve:** Keep the demo gate, incremental rollout, and explicit fallback. Make phases 6-7 a fresh decision after the board and learning loop have produced evidence. Completing phases 0-5 should count as a successful outcome even if Firstmate remains underneath.

2. **Disagree: the alternatives have been rejected too confidently.** An upstream project's philosophy can make configuration awkward, but it is not a technical impossibility. Upstream commit volume does not force you to upgrade. Large source and test counts do not establish that a focused fork is riskier than recreating its operational behavior. A fork can provide substantial control without pruning everything immediately.

   **Suggested fix:** Compare three concrete options: a pinned Firstmate with supported configuration, a maintained fork with a small patch set, and an owned replacement. Score the capabilities actually needed, known failure modes, upgrade obligations, and ability to exit. Keep ownership separate from the claim that replacement will improve output.

3. **Gap: the rollout delays one of the original problems.** One orchestrator per project is an explicit request, yet it appears to arrive only during the replacement phase. Also, section 15 says phases 0-2 solve interleaved replies, while the actual card protocol is phase 3.

   **Suggested fix:** Map every original problem to its first usable release. Test a project-local conversation and orchestration arrangement within Firstmate early, using the supported secondmate behavior where possible. Describe precisely what requires a charter change, configuration change, or code change. Do not make the project workspace experience wait for a rewrite unless the early test proves it must.

4. **Change: establish measurement before changing the workflow.** The baseline currently arrives after board actions, intake, and memory changes. That measures an already modified Firstmate workflow, which is useful for the eventual cutover but cannot show whether the earlier changes helped.

   **Suggested fix:** Start lightweight event capture and a baseline immediately. Record the version of the workflow used by each card. Maintain both an original-workflow baseline and a stable pre-cutover baseline. Ship a simple scorecard and a manual learning proposal before building the complete memory editor.

5. **High-priority gap: the work Mac is a feasibility gate.** The architecture depends on hooks, local services, and memory configuration, but the most constrained machine is tested last. Managed settings can prevent user, project, local, and ordinary plugin hooks from running. That could invalidate the central state-capture design. [Claude Code hook configuration](https://code.claude.com/docs/en/hooks#hook-locations).

   **Suggested fix:** Before the production build, run a small capability check on both Macs and the remote host: permitted hooks, launch method, background service, memory location, pane interaction, and eval availability. Define the minimum common behavior. If work restrictions prevent it, document the reduced behavior explicitly and use an approved integration path.

6. **Disagree: “one tracker” should not mean “no database or index.”** The real requirement is one authoritative workflow. A rebuildable database or search index would not create a second tracker. Conversely, markdown cards, threads, events, proposals, and Git history can become competing authorities even though all are files.

   **Suggested fix:** State which record owns each fact and which records are projections. Keep tasks-axi authoritative during migration. Permit a disposable index when it solves a demonstrated query or performance problem. Reassess prior art on whether its storage can be integrated or become authoritative, rather than rejecting it merely for using a database.

7. **High-priority gap: concurrent writes are not specified.** The orchestrator, scripts, and board can all write cards. Multiple memory writers share Git repositories. Calling the same CLI does not by itself establish locking, atomic updates, or protection from stale edits.

   **Suggested fix:** Verify the existing CLI's concurrency guarantees. Route mutations through one supported write path, serialize writes per project or repository, and attach a revision to editable records. Reject stale board edits with a readable conflict. Define recovery when a card update succeeds but its related event write fails. Use the simplest mechanism that provides those guarantees.

8. **Gap: disk persistence is only part of durability.** The event log is gitignored, and no retention, backup, restore, or partial-write policy is specified. Git history for memory does not automatically protect asks, approvals, threads, and events from machine loss.

   **Suggested fix:** Classify records as authoritative, reconstructible, or disposable. Back up authoritative records within their permitted personal or work boundary. Specify atomic file replacement, handling of a truncated final event, schema migration, and retention. Include one restore exercise that demonstrates recovering an acknowledged card and its pending decision.

9. **High-priority gap: the card lifecycle conflates work state with process state.** A worker can be alive but idle while its card remains in progress. A PR can be open while implementation continues. A card can be blocked on another card without needing the human. The columns also omit clear treatment of cancellation, failure, abandonment, and stale telemetry.

   **Suggested fix:** Persist the underlying lifecycle facts and derive the display column using explicit precedence. Track work status, agent activity, dependencies, and human attention separately. Define completion by card type: shipped change, accepted research, answered question, or completed operation. A stopped agent, an open PR, or a finished retro should not alone determine completion.

10. **Gap: the data model needs identities and relationships beyond a short card tag.** One ask may create several cards, several asks may belong to one card, and one card may have multiple worker attempts or PRs. The same project could also have activity on more than one host.

   **Suggested fix:** Keep short display IDs, backed by stable internal identities for project, ask, card, run, session, and action. Record explicit relationships among them. Define which host owns a project and how ownership moves. Either support multi-host execution deliberately or prohibit concurrent ownership in the first release.

11. **Factual correction: hooks provide observations, not exact complete state.** `Stop` does not run on a user interrupt; API failures use `StopFailure`. `Notification` includes idle and authentication events, and some notifications are delayed. Mapping every notification to “blocked” will produce incorrect states. [Claude Code hooks reference](https://code.claude.com/docs/en/hooks).

   **Suggested fix:** Handle event types individually. Combine hooks with process liveness and reconciliation. Represent unknown or stale state explicitly, including the last observation time. Test interrupted prompts, API failures, permission requests, background activity, and session termination before describing the board's state as reliable.

12. **Change: tags should help people read replies, but should not carry the whole protocol.** Copying an entire reply into every mentioned card can mix unrelated discussions. The final response does not necessarily contain progress updates or earlier questions. A valid-looking tag also does not prove that the ask was actually handled.

   **Suggested fix:** Record ask-to-card disposition through a structured command, with explicit outcomes such as new card, follow-up, chat, or cancelled. Associate message segments with cards deliberately. Retain visible tags for readability. Start Stop-hook enforcement in observation mode; then enable the single retry only if it catches real omissions without frequent false blocks. A failed repair must leave a visible unresolved ask.

13. **High-priority gap: typing into a pane is not durable delivery.** “Add card plus one pane line” has a crash window. The card may exist but never reach the orchestrator, or a retry may cause duplicate handling. A pane may be busy, showing a permission dialog, or no longer hosting the expected session.

   **Suggested fix:** Persist incoming work before acknowledging it. Treat pane input as a wake-up signal carrying an identifier, with the authoritative payload stored on disk. Track pending, acknowledged, and failed delivery. Make retries safe and verify the target session. Ensure a board-created card is not recreated by its own `UserPromptSubmit` event.

14. **Gap: recovery needs acceptance criteria, not just a reconcile command.** “Cards vs panes vs worktrees vs PRs” names the inputs but leaves the hard decisions unspecified. Unexpected or conflicting evidence must not trigger duplicate work, lost work, or destructive cleanup.

   **Suggested fix:** Write a recovery table covering daemon restart, orchestrator crash, worker crash, host reboot, remote disconnect, duplicate events, and a PR merged outside the system. For each, define what can be repaired automatically and what needs a decision. Add end-to-end checks for acknowledged-card survival, duplicate delivery, stale approvals, and preservation of unlanded work.

15. **High-priority gap: migration needs a single active owner.** A “shadow run” is ambiguous. If Firstmate and the replacement both dispatch, answer holds, or land work, the pilot introduces races unrelated to the quality of either system.

   **Suggested fix:** Make shadow mode read-only: it can predict decisions and compare them with the active system, but cannot perform them. Cutover must transfer a recorded project ownership marker. Old processes must refuse new mutations after ownership changes. Document how fallback handles workers and PRs already in flight.

16. **High-priority gap: approval must bind to the exact action being approved.** “PR open and green” does not establish that the approved code is still the PR's current code. A later push could change it. A memory proposal can likewise change between approval and application.

   **Suggested fix:** Bind merge approval to repository, PR, head commit, and intended action. Bind memory approval to the exact proposed diff and base revision. Revalidate at execution and refuse stale approval. Record requested, accepted, executing, succeeded, failed, and uncertain outcomes. Repeating a click must not repeat its side effect.

17. **Gap: the claimed authority guards need an enforceable boundary.** A restricted command is not a guarantee if an agent can perform the same operation through another shell command, API, or tool. This particularly matters for merge, push, filesystem writes, and worktree deletion.

   **Suggested fix:** Write down which restrictions are workflow conventions and which are mechanically enforced. Test alternate tool paths for the important ones. Give each role only the necessary capabilities, and centralize sensitive operations through the approved path where practical. Keep review, permission to execute, and permission to merge as separate decisions.

18. **Change: resolve remote control and work-data boundaries before board actions.** The proposed HTTPS route explicitly conflicts with the existing “one way in” constraint. Also, a board displaying work data on a personal host can violate isolation even if it never copies source code. Titles, prompts, memory, and review findings are work data too.

   **Suggested fix:** Specify permitted data flows and control flows separately. Start remote lanes read-only through the existing saved-machine path. Treat a new control route as an explicit constraint change. Keep work data and its board in the approved work environment unless an approved aggregation arrangement exists. “Same way on both Macs” should mean the same interaction model, with the relevant policy restrictions preserved.

19. **Disagree: captain touches should not be the sole primary success metric.** It mixes corrections, escalations, and rework, which have different meanings. Useful clarification may increase touches while improving quality. Avoiding escalation or splitting work into small cards can make the score look better without helping you.

   **Suggested fix:** Use a small scorecard of accepted outcomes, avoidable human intervention, and escaped defects. Keep the components of the intervention count separate. Sample actual human attention time as well: planned approvals and operating the fleet still consume attention even when excluded from the “unplanned escalation” count. Let you correct the classifier's labels.

20. **Change: strengthen the output and quality guardrails.** Fewer review findings can mean weaker review. More landed cards can mean smaller cards. A seven-day rework window can miss later regressions. Failed or abandoned attempts disappear from a metric whose denominator includes only landed cards.

   **Suggested fix:** Record task category, rough size, acceptance criteria, review coverage, finding severity, and all attempts. Keep short-term rework as one signal while allowing later defects to link back to their origin. Compare similar classes of work. Measure the fleet's maintenance and approval burden alongside project output.

21. **Disagree: ten cards with every metric “no worse” is not a convincing parity test.** It is a useful smoke test, but small samples and different task mixes can make either system look better by chance. An exact no-regression rule can also cause unnecessary switching after harmless fluctuations.

   **Suggested fix:** Treat the first ten cards as a pilot checkpoint. Define tolerances, comparable task categories, and a longer observation window before full cutover. Use immediate rollback triggers for serious failures such as lost work or unauthorized actions. Report uncertainty and the underlying cases instead of reducing the decision to a pass/fail score.

22. **Gap: learning experiments need attribution and reproducibility.** Multiple memory, role, skill, and model changes within the same ten-card window will make it difficult to tell what helped. Pinning only the model and CLI does not identify the complete configuration that produced a result.

   **Suggested fix:** Record a configuration version for each run, including roles, skills, policies, and relevant memory revisions. Test one material change at a time where practical, or measure a named bundle as a bundle. Pin the judge and its rubric too. Use deterministic checks where possible and keep a small set of evaluation cases outside the tuning loop.

23. **Change: the learning loop should capture successful methods as well as mistakes.** The current loop is dominated by correction avoidance. It could miss a debugging technique, a clearer brief, or a useful tool sequence that improves output without preventing a previously observed failure.

   **Suggested fix:** Allow proposals for repeatable successes, supported by evidence and a specific expected effect. Keep “no learning worth saving” as a normal result. Have scripts compute counts and the curator interpret them. Reserve heavier retros for incidents, surprising wins, or a sample of routine cards so the learning machinery does not become a large new cost.

24. **High-priority contradiction: automatic notes are already applied learning.** The requirement says nothing applies without approval, but the notes tier is written directly and can influence later sessions before review. Calling a file a note does not prevent it from changing agent behavior.

   **Suggested fix:** Separate automatically captured evidence from approved operational memory. Store unapproved material where it is not automatically loaded as instruction. If automatic factual notes are desired, define that as an explicit exception with narrow scope and visible status. Instructions and preferences should not enter through that exception.

25. **Change: memory needs scope and validity, not a universal age rule.** A thirty-day decay policy can discard rarely used but important knowledge. Three repetitions can reflect agents copying one another rather than independent confirmation. Global promotion can also move project-specific or work-specific information into the wrong place.

   **Suggested fix:** Give memory a scope, evidence references, validity conditions, and review status. Expire temporary facts according to their conditions; review durable preferences instead of silently deleting them by age. Count independent evidence rather than repeated text. Make cross-project promotion a separate reviewed change, with no automatic promotion across personal and work boundaries.

26. **Disagree: the preceding captain message is not necessarily the reason for a memory.** A discovery may come from tests, code inspection, a later correction, or several cards. Copying the preceding message can create false provenance and unnecessarily duplicate sensitive content.

   **Suggested fix:** Store distinct fields for evidence, rationale, authoring session, and approval. Reference exact messages or observations rather than copying a whole prompt into a commit. Mark imported memories as legacy or unknown provenance when their origin cannot be reconstructed. Do not manufacture certainty to fill the UI.

27. **Gap: post-tool auto-commits are not a complete memory transaction system.** Watching only Write and Edit misses shell edits and other writers. A shared Git index can accidentally bundle concurrent changes. Installed files may also be generated or managed by Nix, where editing the installed copy is the wrong write path.

   **Suggested fix:** Map every editable source to its canonical file and supported application method. Serialize commits per repository and include only the intended change. Treat external edits as changes to reconcile, with honest provenance. Edit dotfiles sources through their normal workflow and rebuild; do not directly modify generated or immutable installed files.

28. **Change: rollback is more than `git revert`.** Reverting an old edit can conflict with newer changes. It also cannot erase the instruction from a running agent's context or undo work the agent already performed because of that instruction.

   **Suggested fix:** Show the proposed reversal and detect conflicts before applying it. Record which sessions loaded the affected revision. Define when they must restart or receive a correction. Distinguish reverting a source file, removing it from future context, and repairing its consequences. Label a memory retired rather than implying history has been erased.

29. **Gap: automatic transcript and provenance storage needs a publication policy.** The proposed private home repo may accumulate raw prompts, local paths, project names, and other information your public-by-default rules prohibit committing. A private repository is not an exception to those rules.

   **Suggested fix:** Keep raw session records local and outside versioned material when they are unsuitable for publication. Commit only deliberately selected, publication-safe memory and metadata. Apply the same rule to eval fixtures and reports. Keep `--no-publish` explicit for eval runs; the documentation describes circumstances in which reports are published. [Claude Code plugin evals](https://code.claude.com/docs/en/plugin-evals).

30. **Factual caveat: one role file does not guarantee equivalent behavior in every execution form.** A custom agent launched with `claude --agent` can replace Claude Code's default system prompt. That is a larger change than adding a role description. The runtime's loading and permission behavior still matters. [Claude Code custom agents](https://code.claude.com/docs/en/sub-agents#invoke-subagents-explicitly).

   **Suggested fix:** Reuse role content where appropriate, but test each supported launch form separately. Verify effective instructions, tools, permissions, model, and memory on startup and resume. Preserve the platform behavior needed by a main-session worker instead of assuming a subagent prompt is a complete replacement for it.

31. **Factual caveats: forked skills and persistent reviewer memory need special care.** A skill using `context: fork` does not receive the current conversation history. Separately, enabling subagent memory automatically enables Read, Write, and Edit, which complicates the proposed read-only reviewer contract. [Skill execution](https://code.claude.com/docs/en/skills#run-skills-in-a-subagent), [subagent memory](https://code.claude.com/docs/en/sub-agents#enable-persistent-memory).

   **Suggested fix:** Keep context-dependent procedures in the current session or pass an explicit brief. Give read-only reviewers approved memory as input without unrestricted memory-writing capability, or enforce a tested memory-only write boundary. Test tool capabilities rather than trusting the role's prose description.

32. **Gap: native memory behavior must be tested through the actual worker launcher.** The plan assumes configuring `autoMemoryDirectory` solves the integration. Current documentation distinguishes main-session and subagent memory and describes auto-memory restrictions for some background or agent-started sessions. [Claude Code memory](https://code.claude.com/docs/en/memory).

   **Suggested fix:** Launch a real worker using the intended herdr path, verify which memory loads, write a harmless test note, restart it, and verify persistence and scope. Repeat on each host class. Give the system an explicit memory-loading path if native behavior does not meet the contract, rather than silently promising coverage it does not have.

33. **Change: several policies are useful starting heuristics, not justified universal rules.** “Over ten files,” “over ten minutes,” and a fixed context threshold are rough proxies for the real concerns: context cost, isolation, lifetime, and recovery. Requiring a new worker for every tiny project edit may also increase the very orchestration burden being measured.

   **Suggested fix:** Keep those values as tunable defaults and measure their effects. Choose execution form by authority, isolation, and lifetime first. Define a small-change path if evidence justifies it. Also choose one owner of worktree creation and cleanup: native `isolation: worktree` and treehouse must not independently manage the same worker lifecycle.

34. **Gap: context reset requires a resumable work record.** A token budget and a stow ritual do not explain what a replacement session must know. The policy also needs to respect the existing rule that only you run `/clear`.

   **Suggested fix:** Define a compact handoff record containing active cards, pending decisions, worker references, current conclusions, and the next action. Verify that a fresh orchestrator can resume from it without reading the entire transcript. Let the agent prepare that handoff and signal the breakpoint; do not silently clear your session.

35. **Gap: capacity and quota checks need reservations and account scope.** Two simultaneous spawns can both pass the same capacity check. If the personal Mac and remote host use the same account, independent per-host quota decisions can also oversubscribe it. A fixed three-percent floor says little about the work still needed to review and finish active cards.

   **Suggested fix:** Reserve capacity atomically when spawning and release it during reconciliation. Define quota ownership per account, not just per host. Budget room for review and recovery, record stale quota data, and establish behavior when quota inspection fails. Start with conservative host allocations if shared coordination would otherwise require new infrastructure.

36. **Addition: make the attention queue more useful than a status board.** Project swimlanes show activity, but the daily value is knowing what you must decide and what will move after that decision. A generic “Needs you” card can still force you to reopen several conversations.

   **Suggested fix:** Each attention item should show the exact question, relevant evidence, the recommended action, its consequence, and whether the answer is still current. Distinguish permission prompts from product decisions. Show whether a board action was recorded, delivered, and completed. Keep secrets out of ordinary card comments. Provide a global attention view alongside project lanes.

37. **Agree fully: keep herdr for the first implementation.** The plan has not established that replacing the terminal layer is necessary to deliver the requested outcomes, and the sidebar is an explicitly valued feature. Polling is a reasonable initial integration when the event path has known limitations.

   **Preserve and refine:** Keep herdr calls behind a small adapter. Make one component responsible for polling each source, prevent overlapping polls, and back off on failure. Add last-successful-update indicators and reconnect recovery for the browser. Avoid promising a precise update latency until it has been measured under a realistic fleet load.

38. **Agree fully: scripts should handle mechanics, and board rendering should consume no model calls.** File watching, state projection, pushing updates, and recording an action do not need an LLM. Keeping those operations deterministic improves both cost and testability.

   **Preserve and refine:** Measure idle daemon load and polling cost separately from model cost. Make rendering continue when agents or model services are unavailable. For the new agent-facing CLI, specify structured output, stable identifiers, meaningful exit codes, and safe retry behavior using the existing AXI standard. Avoid periodic agent wake-ups whose only purpose is refreshing status.

39. **Agree fully: use narrow roles, independent review, and observable completion evidence.** Small definitions with explicit responsibilities are easier to assess than broad persona prompts. Incident-derived regression cases and fresh verification output are especially valuable.

   **Preserve and refine:** Test both outcomes and skill selection. Keep deterministic infrastructure tests separate from model behavior evals. Allow maintenance edits such as correcting a reference without inventing a failing behavioral case solely to satisfy a universal rule. Re-run relevant evals after substantive changes, with a no-skill baseline when measuring the skill's contribution.

40. **Agree fully: memory should earn its place, and improvement should happen through the harness.** The cited AGENTS.md study supports evaluating added instructions and avoiding redundant repository overviews. It also identifies value in instructions for non-standard practices, so it does not justify removing all project guidance. [Evaluating AGENTS.md](https://arxiv.org/abs/2602.11988).

   **Preserve and refine:** Keep concise, relevant instructions, explicit memory approval, pruning, and model-upgrade reevaluation. Evaluate the actual context a role receives, including imports and skills, rather than treating line limits as proof of quality. Measure whether a proposed memory helps on its intended task class.

41. **Change: implementation language and the build estimate are premature commitments.** A preference for Rust and a single binary do not settle the daemon, frontend, packaging, and OS integration choices. The estimate also leaves the recovery, concurrency, compatibility, and approval details above largely unsized.

   **Suggested fix:** Use the compatibility and state-transition spikes to inform the language decision. Re-estimate complete milestones including review, required tests, installation, and rollout. State which parts are narrow prototypes versus supported operation. Make the number of projects in the total explicit, and treat observation periods as minimum evidence windows rather than promised calendar completion dates.

42. **Change: distinguish verified facts from architectural judgment.** Precise star counts, broad collection rankings, and source line counts create an appearance of certainty without proving suitability. “Measurement is the gap in all three” and “best-in-class” are stronger than the evidence presented warrants. Describing close visual copying as automatically fine for a private tool is also unnecessary to this engineering decision.

   **Suggested fix:** Label consequential claims as locally tested, documented, inferred, or unverified, with the tested version and environment. Evaluate the specific role or skill being adopted, not the popularity of its collection. Keep the Linear-inspired interaction and visual direction, but remove unsupported assurances. Pin adopted source revisions and preserve applicable licenses.

43. **Suggested revision to the rollout.** The best next plan should deliver visible value while testing the assumptions that could invalidate later work.

   **Suggested fix:** First, run the host compatibility checks and begin baseline capture. Second, build the already-proposed throwaway board demo and test the project-local conversation arrangement. Third, ship durable intake, action delivery, reconciliation, and the attention queue on top of Firstmate. Fourth, add a small reviewed learning loop and evidence-based memory write-back. Fifth, decide whether a pinned configuration, a focused fork, or a replacement best serves ownership and output; pilot any replacement with one active owner and a tested fallback.
