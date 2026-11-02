import { createFileRoute } from "@tanstack/react-router";

import { HealthPage } from "../components/health/health-page";
import { quotaQuery, signalsQuery } from "../lib/queries";

export const Route = createFileRoute("/health")({
  loader: ({ context }) =>
    Promise.all([
      context.queryClient.ensureQueryData(signalsQuery()),
      context.queryClient.ensureQueryData(quotaQuery()),
    ]),
  component: HealthPage,
});
