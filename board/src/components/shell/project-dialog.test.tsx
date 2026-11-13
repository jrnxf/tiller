import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { ActionError } from "../attention/api";
import { ProjectFailure } from "./project-dialog";

afterEach(cleanup);

const unconfirmed = () =>
  new ActionError(
    504,
    "forward_uncertain",
    "host-d did not confirm in time",
    undefined,
    undefined,
    "host-d",
  );

// An add, archive or delete waits for a clone and its stops, an edit for an
// inline answer (protocol.md 7.4).
test.each([
  [false, "11 minutes"],
  [true, "10 seconds"],
])("an unconfirmed forward with inline %s says it waited %s", (inline, waited) => {
  render(<ProjectFailure error={unconfirmed()} inline={inline} />);
  const notice = screen.getByTestId("sent-not-confirmed");
  expect(notice.textContent).toContain("host-d did not answer within");
  expect(notice.textContent).toContain(waited);
});

test("a peer's refusal names the host that refused it", () => {
  render(
    <ProjectFailure
      error={new ActionError(409, "refused", "project id is taken", undefined, undefined, "host-d")}
    />,
  );
  expect(screen.getByRole("alert").textContent).toBe("host-d refused it: project id is taken");
});
