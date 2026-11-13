import { configure } from "@testing-library/react";

// Builders share one machine, so a render can wait on the CPU for seconds.
// Give each find* query and each test room for that, so load alone never
// fails a test. A real hang still fails, only later.
configure({ asyncUtilTimeout: 5000 });
