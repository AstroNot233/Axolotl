use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

async fn poll(
    client: &Client,
    session: &str,
    operation: &str,
    expected: &str,
) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let value = client
                .tool(
                    "operations_get",
                    json!({"session_id":session,"operation_id":operation}),
                )
                .await;
            let value = value["result"]["structuredContent"].clone();
            if value["status"] == expected {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn deferred_actions_return_immediately_and_results_are_scoped() {
    let backend = Arc::new(TestBackend::default());
    backend.block.store(true, Ordering::SeqCst);
    let server = server_with_backend(true, false, backend.clone()).await;
    let client = connect(&server, false, "Launch owner").await;
    let other = connect(&server, false, "Other").await;
    assert_eq!(
        tool_error(
            &client
                .tool(
                    "instance_launch",
                    json!({"session_id":"invented","instance_id":"instance"})
                )
                .await
        ),
        "session_required"
    );
    assert!(server.snapshot().connection.is_none());
    assert!(backend.calls.lock().unwrap().is_empty());
    let session = control_id(&client.tool("start_session", json!({})).await);
    let response = client
        .tool(
            "instance_launch",
            json!({"session_id":session,"instance_id":"instance"}),
        )
        .await;
    let operation = response["result"]["structuredContent"]["operation_id"]
        .as_str()
        .unwrap();
    poll(&client, &session, operation, "running").await;
    assert_eq!(backend.finished.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&backend.calls.lock().unwrap()[0], Command::InstanceLaunch { instance_id, offline_mode:false } if instance_id == "instance")
    );
    assert_eq!(
        tool_error(
            &other
                .tool(
                    "operations_get",
                    json!({"session_id":session,"operation_id":operation})
                )
                .await
        ),
        "session_required"
    );
    assert_eq!(tool_error(&client.tool("operations_get", json!({"session_id":session,"operation_id":uuid::Uuid::new_v4()})).await), "not_found");
    assert_eq!(
        client
            .tool("operations_list", json!({"session_id":session}))
            .await["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    backend.resume.notify_one();
    assert_eq!(
        poll(&client, &session, operation, "succeeded").await["result"]["executed"],
        true
    );
    client
        .tool("end_session", json!({"session_id":session}))
        .await;
    let next = control_id(&client.tool("start_session", json!({})).await);
    assert_eq!(
        tool_error(
            &client
                .tool(
                    "operations_get",
                    json!({"session_id":next,"operation_id":operation})
                )
                .await
        ),
        "not_found"
    );
    assert!(
        client
            .tool("operations_list", json!({"session_id":next}))
            .await["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.stop().await;
}

#[tokio::test]
async fn deferred_installation_actions_dispatch_and_report_failures() {
    let backend = Arc::new(TestBackend::default());
    let server = server_with_backend(true, false, backend.clone()).await;
    let client = connect(&server, false, "Installation control").await;
    let session = control_id(&client.tool("start_session", json!({})).await);
    for name in [
        "install_job_retry",
        "install_job_resume",
        "install_job_cancel",
    ] {
        let response = client
            .tool(
                name,
                json!({"session_id":session,"job_id":uuid::Uuid::new_v4()}),
            )
            .await;
        let operation = response["result"]["structuredContent"]["operation_id"]
            .as_str()
            .unwrap();
        poll(&client, &session, operation, "succeeded").await;
    }
    assert_eq!(backend.calls.lock().unwrap().len(), 3);
    assert!(
        backend
            .scopes
            .lock()
            .unwrap()
            .iter()
            .all(|scope| scope == &session)
    );
    backend.fail.store(true, Ordering::SeqCst);
    let response = client
        .tool(
            "instance_launch",
            json!({"session_id":session,"instance_id":"missing"}),
        )
        .await;
    let operation = response["result"]["structuredContent"]["operation_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        poll(&client, &session, operation, "failed").await["result"]["error"]["code"],
        "not_found"
    );
    server.stop().await;
}

#[tokio::test]
async fn resource_planning_and_execution_are_deferred_and_use_server_scope() {
    let backend = Arc::new(TestBackend::default());
    let server = server_with_backend(true, false, backend.clone()).await;
    let client = connect(&server, false, "Resource installation").await;
    let session = control_id(&client.tool("start_session", json!({})).await);
    let cases = [
        (
            "resource_prepare_install",
            json!({"instance_id":"instance","source":"modrinth","project_id":"project","release_id":"release","resource_type":"mod"}),
        ),
        (
            "resource_execute_plan",
            json!({"plan_id":uuid::Uuid::new_v4(),"revision":2}),
        ),
        (
            "resource_revise_plan",
            json!({"plan_id":uuid::Uuid::new_v4(),"revision":2,"ignore_dependencies":true,"accept_compatibility_nodes":["node"]}),
        ),
        (
            "content_remove",
            json!({"instance_id":"instance","entry_id":"entry"}),
        ),
        (
            "world_backup",
            json!({"instance_id":"instance","world_id":"save"}),
        ),
        (
            "world_delete",
            json!({"instance_id":"instance","world_id":"save"}),
        ),
        (
            "instance_launch_world",
            json!({"instance_id":"instance","world_id":"save","offline_mode":true}),
        ),
        (
            "instance_join_server",
            json!({"instance_id":"instance","address":"mc.example.com"}),
        ),
        (
            "java_install",
            json!({"vendor":"Vendor Name","major_version":21}),
        ),
        ("download_job_retry", json!({"job_id":uuid::Uuid::new_v4()})),
        (
            "download_job_resume",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        (
            "download_job_cancel",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        (
            "download_job_delete",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        ("download_history_clear", json!({})),
        (
            "install_job_scan_missing_files",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        (
            "install_job_skip_missing",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        (
            "install_job_repair_and_retry",
            json!({"job_id":uuid::Uuid::new_v4()}),
        ),
        (
            "java_test",
            json!({"registered_path":"C:\\Java\\bin\\javaw.exe","major_version":21}),
        ),
        (
            "java_set_default",
            json!({"registered_path":"C:\\Java\\bin\\javaw.exe","major_version":21}),
        ),
    ];
    for (name, mut args) in cases {
        args["session_id"] = json!(session);
        let response = client.tool(name, args).await;
        let id = response["result"]["structuredContent"]["operation_id"]
            .as_str()
            .unwrap();
        assert_eq!(
            poll(&client, &session, id, "succeeded").await["result"]["executed"],
            true
        );
    }
    assert_eq!(backend.calls.lock().unwrap().len(), 19);
    assert!(matches!(
        &backend.calls.lock().unwrap()[0],
        Command::ResourcePrepareInstall {
            source: Source::Modrinth,
            resource_type: InstallKind::Mod,
            ..
        }
    ));
    assert!(
        backend
            .scopes
            .lock()
            .unwrap()
            .iter()
            .all(|scope| scope == &session)
    );
    server.stop().await;
}

#[tokio::test]
async fn ending_session_cancels_deferred_queue_but_drains_active_launch() {
    let backend = Arc::new(TestBackend::default());
    backend.block.store(true, Ordering::SeqCst);
    let server = server_with_backend(true, false, backend.clone()).await;
    let client = connect(&server, false, "Queue").await;
    let session = control_id(&client.tool("start_session", json!({})).await);
    let args = json!({"session_id":session,"instance_id":"instance"});
    let first = client.tool("instance_launch", args.clone()).await;
    let first = first["result"]["structuredContent"]["operation_id"]
        .as_str()
        .unwrap();
    poll(&client, &session, first, "running").await;
    let second = client.tool("instance_launch", args).await;
    let second = second["result"]["structuredContent"]["operation_id"]
        .as_str()
        .unwrap();
    poll(&client, &session, second, "queued").await;
    client
        .tool("end_session", json!({"session_id":session}))
        .await;
    backend.resume.notify_one();
    wait_for(|| backend.finished.load(Ordering::SeqCst) == 1).await;
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn deferred_queue_is_bounded_without_evicting_running_operations() {
    let backend = Arc::new(TestBackend::default());
    backend.block.store(true, Ordering::SeqCst);
    let server = server_with_backend(true, false, backend.clone()).await;
    let client = connect(&server, false, "Bounded queue").await;
    let session = control_id(&client.tool("start_session", json!({})).await);
    for _ in 0..64 {
        let response = client
            .tool(
                "instance_launch",
                json!({"session_id":session,"instance_id":"instance"}),
            )
            .await;
        assert_eq!(response["result"]["isError"], false);
    }
    let response = client
        .tool(
            "instance_launch",
            json!({"session_id":session,"instance_id":"instance"}),
        )
        .await;
    assert_eq!(tool_error(&response), "too_many_operations");
    assert_eq!(
        client
            .tool("operations_list", json!({"session_id":session}))
            .await["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    client
        .tool("end_session", json!({"session_id":session}))
        .await;
    backend.resume.notify_one();
    // No queued launch may enter the backend after the session is revoked.
    wait_for(|| backend.finished.load(Ordering::SeqCst) == 1).await;
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    server.stop().await;
}
