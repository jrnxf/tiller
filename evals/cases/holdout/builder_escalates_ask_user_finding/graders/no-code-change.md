---
type: regex
target:
  source: file
  path: src/export.js
pattern: 'return opts\.json \? JSON\.stringify\(rows\) : rows\.join\("\\n"\);'
---
