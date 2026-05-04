use anyhow::Result;
use std::future::Future;
use tracing::{info, warn};
use zonefile_client::{ProbeResult, ZonefileDownloader};

/// Resolve the actual mode (detailed vs plain) the indexer should run in.
///
/// `prefer_detailed` reflects operator intent (the `--detailed` CLI flag /
/// `DETAILED_MODE` env var). When `false`, the function returns `false`
/// immediately without contacting the API — operator chose plain explicitly.
///
/// When `true`, the function probes the detailed-zonefile endpoint and:
///
/// - returns `true` if the plan supports it,
/// - returns `false` (with a `WARN` log) if the plan rejects detailed
///   downloads (HTTP 403/404), so the cron can complete in plain mode,
/// - propagates `Err` for any other failure (transient outages must remain
///   visible — they should never be silently treated as plan-rejection).
///
/// The probe is taken as a closure for testability so unit tests can
/// substitute synthetic `ProbeResult` values without standing up a mock
/// HTTP server. Production callers pass `|| downloader.probe_detailed_available()`.
pub async fn resolve_mode<F, Fut, E>(prefer_detailed: bool, probe: F) -> Result<bool>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::result::Result<ProbeResult, E>>,
    E: Into<anyhow::Error>,
{
    if !prefer_detailed {
        info!("[MODE] running in mode=plain (operator preference)");
        return Ok(false);
    }

    match probe().await.map_err(Into::into)? {
        ProbeResult::Available => {
            info!("[MODE] running in mode=detailed (probe ok)");
            Ok(true)
        }
        ProbeResult::PlanRejected { status } => {
            warn!(
                status = status,
                "[MODE] detailed requested but plan rejected; falling back to plain mode — set DETAILED_MODE=false to silence this warning"
            );
            Ok(false)
        }
    }
}

/// Convenience wrapper for production callers that own a [`ZonefileDownloader`].
pub async fn resolve_mode_via_downloader(
    prefer_detailed: bool,
    downloader: &ZonefileDownloader,
) -> Result<bool> {
    resolve_mode(prefer_detailed, || downloader.probe_detailed_available()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zonefile_client::ProbeResult;

    // Helper to build an async closure that yields a ready ProbeResult.
    fn ready_probe(
        result: ProbeResult,
    ) -> impl FnOnce() -> std::future::Ready<Result<ProbeResult, anyhow::Error>> {
        move || std::future::ready(Ok(result))
    }

    fn failing_probe(
        msg: &'static str,
    ) -> impl FnOnce() -> std::future::Ready<Result<ProbeResult, anyhow::Error>> {
        move || std::future::ready(Err(anyhow::anyhow!(msg)))
    }

    // Note: "probe is not called when prefer=false" is asserted indirectly by
    // `prefer_false_does_not_invoke_failing_probe` below — if the closure were
    // invoked, its `Err(...)` would propagate and the test's `.unwrap()` would
    // panic. We avoid a captured-bool flag here because async closures
    // capturing mutable locals fail Rust's escape analysis.

    #[tokio::test]
    async fn prefer_true_available_returns_true() {
        let resolved = resolve_mode(true, ready_probe(ProbeResult::Available))
            .await
            .unwrap();
        assert!(resolved);
    }

    #[tokio::test]
    async fn prefer_true_plan_rejected_returns_false() {
        let resolved = resolve_mode(true, ready_probe(ProbeResult::PlanRejected { status: 403 }))
            .await
            .unwrap();
        assert!(!resolved);
    }

    #[tokio::test]
    async fn prefer_true_plan_rejected_404_returns_false() {
        let resolved = resolve_mode(true, ready_probe(ProbeResult::PlanRejected { status: 404 }))
            .await
            .unwrap();
        assert!(!resolved);
    }

    #[tokio::test]
    async fn prefer_true_transient_error_propagates() {
        let err = resolve_mode(true, failing_probe("simulated 500"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("simulated 500"));
    }

    #[tokio::test]
    async fn prefer_false_does_not_invoke_failing_probe() {
        // Even if the probe would fail, prefer=false must not trigger it.
        let resolved = resolve_mode(false, failing_probe("must not be called"))
            .await
            .unwrap();
        assert!(!resolved);
    }

    /// End-to-end fallback test: wires a real `ZonefileDownloader` to a
    /// `wiremock` server that mimics a standard-plan account (403 on the
    /// detailed endpoint) and asserts the production code path resolves
    /// to plain mode without failing the sync.
    ///
    /// This is the regression-canary the spec calls for in §9.3 — it
    /// exercises the real reqwest stack, real Range header, real URL
    /// path, and real probe-to-fallback decision in a single test.
    mod e2e_fallback {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn make_downloader(server: &MockServer) -> ZonefileDownloader {
            let dir = tempfile::tempdir().unwrap();
            ZonefileDownloader::new(server.uri(), "test-token", dir.path()).unwrap()
        }

        #[tokio::test]
        async fn standard_plan_falls_back_to_plain() {
            let server = MockServer::start().await;
            // Standard-plan account: detailed endpoint returns 403.
            Mock::given(method("GET"))
                .and(path("/test-token/get-detailed/full/list/zip"))
                .respond_with(ResponseTemplate::new(403).set_body_string("upgrade required"))
                .mount(&server)
                .await;

            let downloader = make_downloader(&server).await;
            let resolved = resolve_mode_via_downloader(true, &downloader)
                .await
                .expect("plan-rejection must not error — must resolve to plain mode");

            assert!(
                !resolved,
                "DETAILED_MODE=true against a standard-plan account must fall back to plain"
            );
        }

        #[tokio::test]
        async fn pro_plan_uses_detailed() {
            let server = MockServer::start().await;
            // Pro-plan account: detailed endpoint returns 200.
            Mock::given(method("GET"))
                .and(path("/test-token/get-detailed/full/list/zip"))
                .respond_with(ResponseTemplate::new(200).set_body_string(""))
                .mount(&server)
                .await;

            let downloader = make_downloader(&server).await;
            let resolved = resolve_mode_via_downloader(true, &downloader)
                .await
                .expect("Available probe must not error");

            assert!(resolved, "DETAILED_MODE=true on Pro plan must resolve to detailed mode");
        }

        #[tokio::test]
        async fn transient_5xx_propagates_does_not_fall_back() {
            let server = MockServer::start().await;
            // Real outage: detailed endpoint returns 503.
            Mock::given(method("GET"))
                .and(path("/test-token/get-detailed/full/list/zip"))
                .respond_with(ResponseTemplate::new(503))
                .mount(&server)
                .await;

            let downloader = make_downloader(&server).await;
            let err = resolve_mode_via_downloader(true, &downloader)
                .await
                .expect_err("5xx must NOT silently fall back to plain — sync must fail loudly");

            assert!(
                err.to_string().contains("503"),
                "error should mention the upstream status, got: {err}"
            );
        }
    }
}
