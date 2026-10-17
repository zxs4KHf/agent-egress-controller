use aec_core::{
    cooperation::{evaluate_dns, probe_fixed_dns},
    http::{HttpClient, HttpError, Request, Response, Route},
};
use serde_json::json;
use std::{sync::Mutex, time::Duration};
fn authority(ecs: Option<&str>) -> serde_json::Value {
    let mut answer = vec![
        json!({"name":"abcdef12.test.dnscheck.tools.","type":16,"TTL":1,"data":"\"FROM: 192.0.2.8#12345 (udp)\""}),
        json!({"name":"abcdef12.test.dnscheck.tools.","type":16,"TTL":1,"data":"\"ID: 1234\""}),
    ];
    if let Some(ecs) = ecs {
        answer.push(json!({"name":"abcdef12.test.dnscheck.tools.","type":16,"TTL":1,"data":format!("\"ECS: {ecs}\"")}));
    }
    json!({"Status":0,"TC":false,"Question":[{"Name":"abcdef12.test.dnscheck.tools.","Qtype":16,"Qclass":1}],"Answer":answer})
}
#[test]
fn authority_nonce_proves_only_that_query_and_never_turns_missing_evidence_green() {
    let reply = authority(None);
    assert_eq!(evaluate_dns(&reply, "abcdef12", 500)["status"], "pass");
    assert_eq!(
        evaluate_dns(&authority(Some("203.0.113.0/24")), "abcdef12", 500)["status"],
        "warn"
    );
    assert_eq!(
        evaluate_dns(&authority(Some("0.0.0.0/0")), "abcdef12", 500)["status"],
        "pass"
    );
    assert_eq!(evaluate_dns(&reply, "00000000", 500)["status"], "unknown");
    assert_eq!(evaluate_dns(&reply, "abcdef12", 20000)["status"], "unknown");
    let mut truncated = reply.clone();
    truncated["TC"] = true.into();
    assert_eq!(
        evaluate_dns(&truncated, "abcdef12", 500)["status"],
        "unknown"
    );
    let mut partial = reply;
    partial["Answer"].as_array_mut().unwrap().remove(0);
    assert_eq!(evaluate_dns(&partial, "abcdef12", 500)["status"], "unknown");
}
struct Boundary(Mutex<Vec<Request>>);
impl HttpClient for Boundary {
    fn send(&self, req: &Request) -> Result<Response, HttpError> {
        self.0.lock().unwrap().push(req.clone());
        Err(HttpError::Timeout)
    }
}
#[test]
fn fixed_doh_failure_has_no_direct_or_ordinary_retry() {
    let http = Boundary(Mutex::new(vec![]));
    let evidence = probe_fixed_dns(&http, 17328, "abcdef12");
    assert_eq!(evidence["status"], "unknown");
    let requests = http.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].route, Route::Proxy(17328));
    assert_eq!(requests[0].total_timeout, Duration::from_secs(8));
    assert!(requests[0].url.contains("abcdef12.test.dnscheck.tools"));
}
