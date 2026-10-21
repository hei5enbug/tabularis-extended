use super::http_fixture::RegistryServer;
use std::{error::Error as StdError, fmt::Debug, time::Duration};
use tabularium_sdk::{Client, Error};

struct RequestBatch {
    succeeded: Vec<bool>,
    routes: Vec<String>,
}

fn print_error_chain<E: Debug>(index: usize, error: &Error<E>) {
    match error {
        Error::ErrorResponse(_)
        | Error::UnexpectedResponse(_)
        | Error::InvalidResponsePayload(_, _) => {
            eprintln!(
                "request[{index}] response error: status={:?}; payload and headers omitted",
                error.status()
            );
        }
        _ => eprintln!("request[{index}] error={error:?}"),
    }
    let mut source = error.source();
    let mut depth = 0;
    while let Some(cause) = source {
        eprintln!("request[{index}] source[{depth}] debug={cause:?}; display={cause}");
        source = cause.source();
        depth += 1;
        if depth == 16 {
            break;
        }
    }
}

async fn collect_sequential_requests(server: &RegistryServer) -> RequestBatch {
    let mut succeeded = Vec::with_capacity(20);
    for index in 0..20 {
        let http = crate::proxy::build_reqwest_012_client(None, Duration::from_secs(30)).unwrap();
        let client = Client::new_with_client(&server.base, http);
        let result = client.get_plugin().slug("fixture-theme").send().await;
        if let Err(error) = &result {
            print_error_chain(index, error);
        }
        succeeded.push(result.is_ok());
    }
    let routes = server.requests.lock().unwrap().clone();
    eprintln!("fixture routes={routes:?}");
    RequestBatch { succeeded, routes }
}

#[tokio::test]
async fn 동일한_레지스트리에_순차_요청한_스무_응답을_모두_읽는다() {
    // Given
    let server = RegistryServer::new(vec![]);
    // When
    let batch = collect_sequential_requests(&server).await;
    // Then
    assert_eq!(batch.succeeded.len(), 20);
    assert_eq!(batch.routes.len(), 20);
    assert!(batch
        .routes
        .iter()
        .all(|route| route.trim_end_matches('/') == "/api/plugins/fixture-theme"));
    assert!(
        batch.succeeded.iter().all(|success| *success),
        "Request success flags: {:?}",
        batch.succeeded
    );
}
