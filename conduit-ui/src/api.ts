/**
 * Public API surface for the operator UI.
 * Implementation lives in `lib/adminClient.ts` (injectable pure HTTP client).
 */
export {
  api,
  adminUrl,
  getAdminBase,
  setAdminBase,
  createAdminApi,
  health,
  providers,
  routes,
  keys,
  usage,
  pricing,
  traces,
  AdminClientError,
} from "./lib/adminClient";

export type {
  Provider,
  CreateProviderBody,
  Route,
  CreateRouteBody,
  DownstreamKey,
  CreateKeyResponse,
  UsageSummaryEntry,
  UsageSummaryResponse,
  PricingRow,
  HealthResponse,
  TraceIndexRow,
  TraceListResponse,
  ReplayPlan,
} from "./lib/adminClient";
