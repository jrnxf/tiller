import { charCount, isTooLong } from "../../lib/limits";

// Says why Send is off when a field is over the daemon's cap, so the owner sees
// the limit before sending and never meets the raw 400. Renders nothing while
// the text fits.
export function LimitNote({ what, text, limit }: { what: string; text: string; limit: number }) {
  if (!isTooLong(text, limit)) return null;
  return (
    <p className="limit-note" role="status">
      The {what} is {charCount(text).toLocaleString("en-US")} characters. The limit is{" "}
      {limit.toLocaleString("en-US")}.
    </p>
  );
}
