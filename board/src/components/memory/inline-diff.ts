// A rewrite shown the way suggestion mode shows it in a shared document: the
// old words struck, the new ones tinted, the rest as they were. Word by word,
// by longest common subsequence (the approved mocks' `inlineDiff`).
export interface DiffPart {
  kind: "same" | "del" | "ins";
  text: string;
}

export function inlineDiff(before: string, after: string): DiffPart[] {
  const a = before.split(" ");
  const b = after.split(" ");
  const n = a.length;
  const m = b.length;
  // lcs[i * (m + 1) + j]: how many words a[i..] and b[j..] share in order.
  const lcs = new Int32Array((n + 1) * (m + 1));
  const at = (i: number, j: number) => lcs[i * (m + 1) + j] ?? 0;
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i * (m + 1) + j] =
        a[i] === b[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
    }
  }
  const out: DiffPart[] = [];
  let dels: string[] = [];
  let ins: string[] = [];
  const push = (kind: DiffPart["kind"], words: string[]) => {
    if (words.length) out.push({ kind, text: words.join(" ") });
  };
  const flush = () => {
    push("del", dels);
    push("ins", ins);
    dels = [];
    ins = [];
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    const word = a[i] ?? "";
    if (word === b[j]) {
      flush();
      const last = out.at(-1);
      if (last?.kind === "same") last.text += ` ${word}`;
      else out.push({ kind: "same", text: word });
      i++;
      j++;
    } else if (at(i + 1, j) >= at(i, j + 1)) {
      dels.push(word);
      i++;
    } else {
      ins.push(b[j] ?? "");
      j++;
    }
  }
  dels.push(...a.slice(i));
  ins.push(...b.slice(j));
  flush();
  return out;
}
