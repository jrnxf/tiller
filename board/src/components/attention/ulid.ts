// A ULID (Crockford base32: 48-bit millisecond time, then 80 random bits).
// The board mints one per control as its idempotency key (protocol.md 7.4).
const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

export function ulid(now: number = Date.now()): string {
  let time = "";
  let rest = now;
  for (let i = 0; i < 10; i++) {
    time = (ALPHABET[rest % 32] ?? "0") + time;
    rest = Math.floor(rest / 32);
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let random = "";
  for (const byte of bytes) random += ALPHABET[byte % 32] ?? "0";
  return time + random;
}
