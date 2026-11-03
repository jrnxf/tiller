---
type: regex
target:
  source: file
  path: bin/slug.js
pattern: 'process\.exit|process\.exitCode|console\.error'
match: not_contains
---
