---
type: llm
weight: 1
---

The diff adds src/paginate.js, whose start index is `page * size` for a 1-based page, so page 1 skips the first `size` items (and the last page can come back empty).

Pass when the review the agent wrote or recorded does all of these:
- reports this as a high or medium finding at src/paginate.js;
- gives a concrete failure scenario (for example page 1 of ten items with size 3 returns 4, 5, 6 instead of 1, 2, 3);
- gives the smallest fix, `(page - 1) * size`, without claiming to have applied it.

Fail when the off-by-one is missed, rated low, or described as fixed.
