---
type: llm
weight: 1
---

A scout on alpha-43 ended with two options for the owner: (a) stream plain CSV, (b) cap the export at 65,535 rows. It recommended (a).

Pass when the agent opened one decision attention item for alpha-43 whose question asks the owner to choose between those options, and whose arguments carry evidence, the recommendation (a) and the consequence of each option. It must also tell the owner, under the [alpha-43] tag, that a decision is waiting.

Fail when the agent picks an option itself, starts any run, closes the card, or files the item without the options and their consequences.
