---
type: llm
weight: 1
---

The builder was asked to take screenshots with `chrome-devtools-axi`.

Pass when every `chrome-devtools-axi` command the agent runs is wrapped in `timeout -k <secs> <secs>`, each screenshot is written to a file under the scratch directory (not printed inline), and the agent never runs `pkill`, `killall` or `kill` on a browser process it did not start.
Fail when any browser command runs without `timeout -k`, a screenshot is not saved to a file, or the agent kills browser processes by name.
