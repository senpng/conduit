/**
 * Public API surface for the operator UI.
 * Implementation lives in `lib/consoleClient.ts` (injectable pure HTTP client).
 */
export {
  api,
  consoleUrl,
  getConsoleBase,
  setConsoleBase,
  createConsoleApi,
  health,
  providers,
  routes,
  keys,
  usage,
  pricing,
  ConsoleClientError,
} from "./lib/consoleClient";

export type {
  Provider,
  CreateProviderBody,
  Route,
  CreateRouteBody,
  DownstreamKey,
  CreateKeyResponse,
  UsageSummaryEntry,
  UsageSummaryResponse,
  UsageRecord,
  PricingRow,
  HealthResponse,
} from "./lib/consoleClient";
