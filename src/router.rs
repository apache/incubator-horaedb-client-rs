use std::collections::HashMap;

use async_trait::async_trait;
use ceresdbproto::storage::{Endpoint as EndpointPb, Route, RouteRequest, RouteResponse};
use dashmap::DashMap;

use crate::{
    errors::Result,
    model::route::Endpoint,
    rpc_client::{RpcClient, RpcContext},
    Error,
};

#[async_trait]
pub trait Router {
    /// Route metrics.
    ///
    /// we can set force_refresh for getting endpoints from server directly
    /// and update cache after.
    /// Otherwise, it will get target endpoints  from cache(if exist) firstly
    /// and get from server only when they not exist in local cache.
    async fn route(
        &self,
        metrics: &[String],
        ctx: &RpcContext,
        force_refresh: bool,
    ) -> Result<Vec<Option<Endpoint>>>;
}

struct RouterImpl<R: RpcClient> {
    cache: DashMap<String, Endpoint>,
    rpc_client: R,
}

impl<R: RpcClient> RouterImpl<R> {
    pub fn new(rpc_client: R) -> Self {
        Self {
            cache: DashMap::new(),
            rpc_client,
        }
    }
}

#[async_trait]
impl<R: RpcClient> Router for RouterImpl<R> {
    async fn route(
        &self,
        metrics: &[String],
        ctx: &RpcContext,
        force_refresh: bool,
    ) -> Result<Vec<Option<Endpoint>>> {
        let mut target_endpoints = vec![None; metrics.len()];

        // Find from cache firstly (if force_refresh is false),
        // and collect misses.
        let misses = if !force_refresh {
            let mut misses = HashMap::new();
            for (idx, metric) in metrics.into_iter().enumerate() {
                match self.cache.get(metric) {
                    Some(pair) => {
                        target_endpoints[idx] = Some(pair.value().clone());
                    }

                    None => {
                        // There should not be duplicated metric in metrics
                        if let Some(_) = misses.insert(metric.clone(), idx) {
                            return Err(Error::Unknown(format!(
                                "Route duplicated metric:{}",
                                metric
                            )));
                        }
                    }
                }
            }
            misses
        } else {
            metrics
                .into_iter()
                .enumerate()
                .map(|(idx, m)| (m.clone(), idx))
                .collect()
        };

        // Get endpoints of misses from remote.
        let mut remote_req = RouteRequest::default();
        let miss_metrics = misses.iter().map(|(m, _)| m.clone()).collect();
        remote_req.set_metrics(miss_metrics);
        let remote_resp = self.rpc_client.route(ctx, &remote_req).await?;

        // Fill miss endpoint and update cache.
        for route in remote_resp.routes {
            // Endpoint may be none, and not cache it when it is none.
            if route.endpoint.is_none() {
                continue;
            }

            // Impossible to get none.
            let idx = misses.get(&route.metric).ok_or_else(|| {
                Error::Unknown(format!("Unknown metric:{} in response", route.metric))
            })?;
            let endpoint: Endpoint = route.endpoint.unwrap().into();
            self.cache.insert(route.metric, endpoint.clone());
            target_endpoints[*idx] = Some(endpoint);
        }

        Ok(target_endpoints)
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use dashmap::DashMap;

    use super::{Router, RouterImpl};
    use crate::{
        model::route::Endpoint,
        rpc_client::{MockRpcClient, RpcContext},
    };

    #[tokio::test]
    async fn test_basic_flow() {
        // Init mock route table
        let metric1 = "metric1".to_string();
        let metric2 = "metric2".to_string();
        let endpoint1 = Endpoint::new("192.168.0.1".to_string(), 11);
        let endpoint2 = Endpoint::new("192.168.0.2".to_string(), 12);
        let endpoint3 = Endpoint::new("192.168.0.3".to_string(), 13);
        let endpoint4 = Endpoint::new("192.168.0.4".to_string(), 14);

        // Init mock client with route1 and route2
        let route_table = Arc::new(DashMap::default());
        let mock_rpc_client = MockRpcClient {
            route_table: route_table.clone(),
        };
        mock_rpc_client
            .route_table
            .insert(metric1.clone(), endpoint1.clone());
        mock_rpc_client
            .route_table
            .insert(metric2.clone(), endpoint2.clone());

        // Follow these steps to check wether cache is used or not:
        // route --> change route_table --> route again.
        let ctx = RpcContext::new("test".to_string(), "".to_string());
        let metrics = vec![metric1.clone(), metric2.clone()];
        let route_client = RouterImpl::new(mock_rpc_client);
        let route_res1 = route_client
            .route(&metrics, &ctx, false)
            .await
            .unwrap();
        let a =  
        assert_eq!(
            &endpoint1,
            route_res1.get(0).unwrap().as_ref().unwrap()
        );
        assert_eq!(
            &endpoint2,
            route_res1.get(1).unwrap().as_ref().unwrap()
        );

        let route_res2 = route_client
            .route(&metrics, &ctx, false)
            .await
            .unwrap();
        assert_eq!(
            &endpoint1,
            route_res2.get(0).unwrap().as_ref().unwrap()
        );
        assert_eq!(
            &endpoint2,
            route_res2.get(1).unwrap().as_ref().unwrap()
        );

        let route_res3 = route_client
            .route(&metrics, &ctx, true)
            .await
            .unwrap();
        assert_eq!(
            &endpoint3,
            route_res3.get(0).unwrap().as_ref().unwrap()
        );
        assert_eq!(
            &endpoint4,
            route_res3.get(1).unwrap().as_ref().unwrap()
        );
    }
}