import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import type { ErrorComponentProps } from "@tanstack/react-router";
import { useRouter } from "@tanstack/react-router";
import { useCallback } from "react";

import { failureReason, useAutoRetry } from "./section";

// A screen whose reads failed: it says so inside the shell, which stays up
// with whatever else did load, and loads the screen again by itself. As the
// root route's error screen it stands alone, since the shell is what failed.
export function ScreenUnavailable({ error, reset }: ErrorComponentProps) {
  const router = useRouter();
  const queries = useQueryErrorResetBoundary();
  const retry = useCallback(() => {
    queries.reset();
    reset();
    void router.invalidate();
  }, [queries, reset, router]);
  useAutoRetry(retry, error);

  return (
    <div className="empty" role="status" data-testid="screen-unavailable">
      <div>
        <b>This screen could not be loaded</b>
        {failureReason(error)}
        <br />
        <button type="button" className="btn" onClick={retry}>
          Retry
        </button>
      </div>
    </div>
  );
}
