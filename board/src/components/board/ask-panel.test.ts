import { expect, test } from "vitest";

import { deliveryText } from "./ask-panel";

const SENT = "2026-11-02T12:00:00.000Z";
const note = {
  sent_at: SENT,
  delivered_at: null,
  seen_at: null,
  failed_at: null,
  failure_reason: null,
};

test("the Delivery line says whether the ask reached the orchestrator", () => {
  expect(deliveryText(null, "prompt")).toBe("Typed in the orchestrator's pane");
  expect(deliveryText(null, "board")).toBe("Not sent: no orchestrator ran when it was asked");
  expect(deliveryText({ ...note, state: "pending" }, "board")).toMatch(
    /^Waiting to deliver, sent /,
  );
  expect(deliveryText({ ...note, state: "delivered", delivered_at: SENT }, "board")).toMatch(
    /^Delivered \w{3} /,
  );
  expect(
    deliveryText({ ...note, state: "acked", delivered_at: SENT, seen_at: SENT }, "board"),
  ).toMatch(/^Delivered .+, seen /);
  expect(
    deliveryText(
      { ...note, state: "failed", failed_at: SENT, failure_reason: "the pane is gone" },
      "script",
    ),
  ).toBe("Not delivered: the pane is gone");
});
