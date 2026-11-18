// An orchestrator's terminal line starts with its tag and the project's name,
// "[chat] alpha: " or "[alpha-41] alpha: ", which intake reads (P40). A
// person reads the words after them: the chat and the status lines drop them.
const TAG = /^[ \t]*\[(?:chat|[a-z][a-z0-9-]*-\d+)\][ \t]*(?:[a-z0-9][\w.-]*:[ \t]+)?/gm;

export function untagged(text: string): string {
  return text.replace(TAG, "");
}
